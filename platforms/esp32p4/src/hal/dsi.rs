//! Bare-metal ESP32-P4 MIPI-DSI binding backed by `esp-hal`.

use barracuda_peripheral::display::DsiPanelConfig;
use esp_hal::{
    gpio::{Level, Output, OutputConfig, OutputPin},
    mipi_dsi::{Config as HalDsiConfig, ConfigError as HalDsiConfigError, DataLanes, MipiDsi},
    peripherals::{MIPI_DSI, VDMA_CH0},
};

const MAX_ROW_BYTES: usize = 720 * 2;

/// Failure while constructing or operating the ESP32-P4 DSI data plane.
#[derive(Debug)]
pub enum DsiConfigError {
    /// This binding supports the ESP32-P4 two-lane D-PHY configuration.
    UnsupportedLaneCount,
    /// The Board's declared D-PHY rail differs from the rail managed by `esp-hal`.
    UnsupportedPhyPower,
    /// The requested panel geometry exceeds the Tab5 command buffer.
    UnsupportedFrameSize,
    /// The DSI configuration was rejected by `esp-hal`.
    Dsi(HalDsiConfigError),
    /// The host has already been initialized.
    AlreadyInitialized,
    /// Pixel coordinates or payload length do not describe a valid rectangle.
    InvalidDraw,
}

/// ESP32-P4 command-mode DSI host with a GPIO-controlled backlight.
pub struct DsiHost {
    dsi: Option<MIPI_DSI<'static>>,
    vdma: Option<VDMA_CH0<'static>>,
    bus: Option<MipiDsi<'static>>,
    backlight: Output<'static>,
    width: u16,
    height: u16,
    brightness: u8,
    enabled: bool,
}

/// Constructs the ESP32-P4 DSI host from the exact `esp-hal` ownership tokens.
pub fn dsi_host(
    dsi: MIPI_DSI<'static>,
    vdma: VDMA_CH0<'static>,
    backlight_pin: impl OutputPin + 'static,
    data_lanes: u8,
    phy_power_channel: u8,
    phy_power_millivolts: u16,
) -> Result<DsiHost, DsiConfigError> {
    if data_lanes != 2 {
        return Err(DsiConfigError::UnsupportedLaneCount);
    }
    // esp-hal owns ESP32-P4 PMU LDO channel 3 and programs it to 2.5 V when
    // MipiDsi::new is called. Validate the Board declaration against that API.
    if phy_power_channel != 3 || phy_power_millivolts != 2_500 {
        return Err(DsiConfigError::UnsupportedPhyPower);
    }

    Ok(DsiHost {
        dsi: Some(dsi),
        vdma: Some(vdma),
        bus: None,
        backlight: Output::new(backlight_pin, Level::Low, OutputConfig::default()),
        width: 0,
        height: 0,
        brightness: 255,
        enabled: false,
    })
}

impl barracuda_peripheral::display::DsiHost for DsiHost {
    type Error = DsiConfigError;

    fn initialize(&mut self, config: DsiPanelConfig) -> Result<(), Self::Error> {
        let dsi = self.dsi.take().ok_or(DsiConfigError::AlreadyInitialized)?;
        let vdma = self.vdma.take().ok_or(DsiConfigError::AlreadyInitialized)?;
        if usize::from(config.timing.width) * 2 > MAX_ROW_BYTES {
            return Err(DsiConfigError::UnsupportedFrameSize);
        }

        let bus = MipiDsi::new(
            dsi,
            vdma,
            HalDsiConfig::default()
                .with_num_data_lanes(DataLanes::_2)
                .with_lane_bit_rate_mbps(config.lane_bit_rate_mbps as f32),
        )
        .map_err(DsiConfigError::Dsi)?;
        self.width = config.timing.width;
        self.height = config.timing.height;
        self.bus = Some(bus);
        self.enabled = false;
        self.apply_backlight()
    }

    fn write_command(&mut self, command: u8, data: &[u8]) -> Result<(), Self::Error> {
        self.bus
            .as_mut()
            .ok_or(DsiConfigError::AlreadyInitialized)?
            .dbi(0)
            .write_cmd(command, data)
            .map_err(|_| DsiConfigError::InvalidDraw)
    }

    fn draw_rgb565(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        pixels: &[u16],
    ) -> Result<(), Self::Error> {
        let right = x.checked_add(width).ok_or(DsiConfigError::InvalidDraw)?;
        let bottom = y.checked_add(height).ok_or(DsiConfigError::InvalidDraw)?;
        let expected = usize::from(width)
            .checked_mul(usize::from(height))
            .ok_or(DsiConfigError::InvalidDraw)?;
        if right > self.width || bottom > self.height || pixels.len() != expected {
            return Err(DsiConfigError::InvalidDraw);
        }
        let bus = self
            .bus
            .as_mut()
            .ok_or(DsiConfigError::AlreadyInitialized)?;
        let x_end = right - 1;
        let y_end = bottom - 1;
        let mut dbi = bus.dbi(0);
        dbi.write_cmd(
            0x2a,
            &[(x >> 8) as u8, x as u8, (x_end >> 8) as u8, x_end as u8],
        )
        .map_err(|_| DsiConfigError::InvalidDraw)?;
        dbi.write_cmd(
            0x2b,
            &[(y >> 8) as u8, y as u8, (y_end >> 8) as u8, y_end as u8],
        )
        .map_err(|_| DsiConfigError::InvalidDraw)?;
        let mut row = [0u8; MAX_ROW_BYTES];
        for (line_index, source) in pixels.chunks_exact(usize::from(width)).enumerate() {
            let row_bytes = usize::from(width) * 2;
            let target = &mut row[..row_bytes];
            for (destination, pixel) in target.as_chunks_mut::<2>().0.iter_mut().zip(source) {
                destination.copy_from_slice(&pixel.to_be_bytes());
            }
            dbi.write_cmd(if line_index == 0 { 0x2c } else { 0x3c }, target)
                .map_err(|_| DsiConfigError::InvalidDraw)?;
        }
        Ok(())
    }

    fn set_display_enabled(&mut self, enabled: bool) -> Result<(), Self::Error> {
        self.command(if enabled { 0x29 } else { 0x28 })?;
        self.enabled = enabled;
        self.apply_backlight()
    }

    fn set_sleep(&mut self, sleep: bool) -> Result<(), Self::Error> {
        self.command(if sleep { 0x10 } else { 0x11 })?;
        self.enabled = !sleep;
        self.apply_backlight()
    }

    fn set_backlight(&mut self, brightness: u8) -> Result<(), Self::Error> {
        self.brightness = brightness;
        self.apply_backlight()
    }
}

impl DsiHost {
    fn command(&mut self, command: u8) -> Result<(), DsiConfigError> {
        self.bus
            .as_mut()
            .ok_or(DsiConfigError::AlreadyInitialized)?
            .dbi(0)
            .write_cmd(command, &[])
            .map_err(|_| DsiConfigError::InvalidDraw)
    }

    fn apply_backlight(&mut self) -> Result<(), DsiConfigError> {
        if self.enabled && self.brightness != 0 {
            self.backlight.set_high();
        } else {
            self.backlight.set_low();
        }
        Ok(())
    }
}

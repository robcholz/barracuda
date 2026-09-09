//! Revision-aware MIPI DSI display implementation for the M5Stack Tab5.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_gt911::Gt911;
use barracuda_driver_ili9881c::INIT_COMMANDS as ILI9881C_COMMANDS;
use barracuda_driver_pi4ioe5v6408::{Error as ExpanderError, Pi4ioe5v6408};
use barracuda_driver_st712x::{
    Model as St712xModel, ST7121_INIT_COMMANDS, ST7123_INIT_COMMANDS, St712x,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    display::{
        Display, DisplayDescriptor, DisplayFeatures, DisplayOrientation, DisplayPower,
        DisplayTechnology, DsiHost, DsiPanelConfig, DsiVideoTiming, PixelFormat, RefreshRequest,
    },
};
use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{Dimensions, OriginDimensions, Size},
    pixelcolor::{Rgb565, Rgb888, RgbColor},
    primitives::Rectangle,
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

const WIDTH: u16 = 720;
const HEIGHT: u16 = 1280;
const CONTROL_EXPANDER_ADDRESS: u8 = 0x43;
const DISPLAY_RESET_PIN: u8 = 4;

/// Board-owned addresses used to identify the shipping Tab5 display revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tab5DsiDisplayConfig {
    gt911_address: u8,
    st712x_address: u8,
}

impl Tab5DsiDisplayConfig {
    /// Creates automatic panel-detection configuration.
    #[must_use]
    pub const fn new(gt911_address: u8, st712x_address: u8) -> Self {
        Self {
            gt911_address,
            st712x_address,
        }
    }
}

/// Move-only DSI host and shared control-bus view consumed by this implementation.
pub struct Tab5DsiDisplayBindings<HOST, I2C, DELAY> {
    host: HOST,
    i2c: I2C,
    delay: DELAY,
}

impl<HOST, I2C, DELAY> Tab5DsiDisplayBindings<HOST, I2C, DELAY> {
    /// Combines the DSI data plane with the revision-detection bus.
    #[must_use]
    pub const fn new(host: HOST, i2c: I2C, delay: DELAY) -> Self {
        Self { host, i2c, delay }
    }
}

/// Display initialization failure.
#[derive(Debug)]
pub enum Tab5DsiDisplayInitError<BusError, HostError> {
    /// One or both configured I2C addresses are invalid.
    InvalidAddresses,
    /// Neither supported Tab5 panel revision responded with a valid identity.
    UnsupportedPanel,
    /// The final panel probe failed at the I2C layer.
    Bus(BusError),
    /// The private Tab5 panel-reset expander could not be configured.
    Expander(ExpanderError<BusError>),
    /// The Platform DSI host rejected panel initialization.
    Host(HostError),
}

/// Runtime display operation failure.
#[derive(Debug)]
pub enum Tab5DsiDisplayError<HostError> {
    /// The Platform DSI host rejected the operation.
    Host(HostError),
    /// Tab5 currently exposes only its native portrait orientation.
    UnsupportedOrientation,
    /// Pixel data did not fill the requested rectangle.
    InvalidPixelCount,
}

/// Initialized Tab5 display implementing Barracuda's stable display API.
pub struct Tab5DsiDisplay<HOST> {
    host: HOST,
}

/// Static factory used by generated Board composition.
pub struct Tab5DsiDisplayImplementation<HOST, I2C, DELAY>(
    PhantomData<HOST>,
    PhantomData<I2C>,
    PhantomData<DELAY>,
);

impl<HOST, I2C, DELAY> PeripheralImplementation for Tab5DsiDisplayImplementation<HOST, I2C, DELAY>
where
    HOST: DsiHost + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Tab5DsiDisplayBindings<HOST, I2C, DELAY>;
    type Config = Tab5DsiDisplayConfig;
    type Peripheral = Tab5DsiDisplay<HOST>;
    type Error = Tab5DsiDisplayInitError<I2C::Error, HOST::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        if config.gt911_address > 0x7f
            || config.st712x_address > 0x7f
            || config.gt911_address == config.st712x_address
        {
            return Err(Tab5DsiDisplayInitError::InvalidAddresses);
        }

        Pi4ioe5v6408::new(&mut bindings.i2c, CONTROL_EXPANDER_ADDRESS)
            .map_err(Tab5DsiDisplayInitError::Expander)?
            .pulse_reset_low(DISPLAY_RESET_PIN, 10, 120, &mut bindings.delay)
            .map_err(Tab5DsiDisplayInitError::Expander)?;

        let panel = detect_panel(
            &mut bindings.i2c,
            config.gt911_address,
            config.st712x_address,
        )
        .map_err(|error| match error {
            DetectError::Bus(error) => Tab5DsiDisplayInitError::Bus(error),
            DetectError::UnsupportedPanel => Tab5DsiDisplayInitError::UnsupportedPanel,
        })?;
        bindings
            .host
            .initialize(panel.config())
            .map_err(Tab5DsiDisplayInitError::Host)?;
        panel
            .write_init_commands(&mut bindings.host, &mut bindings.delay)
            .map_err(Tab5DsiDisplayInitError::Host)?;
        bindings
            .host
            .set_display_enabled(true)
            .map_err(Tab5DsiDisplayInitError::Host)?;
        Ok(Tab5DsiDisplay {
            host: bindings.host,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelRevision {
    Ili9881c,
    St7123,
    St7121,
}

enum DetectError<BusError> {
    Bus(BusError),
    UnsupportedPanel,
}

impl PanelRevision {
    fn config(self) -> DsiPanelConfig {
        match self {
            Self::Ili9881c => DsiPanelConfig {
                lane_bit_rate_mbps: 1_000,
                dpi_clock_mhz: 60,
                timing: DsiVideoTiming {
                    width: WIDTH,
                    height: HEIGHT,
                    hsync_back_porch: 140,
                    hsync_pulse_width: 40,
                    hsync_front_porch: 40,
                    vsync_back_porch: 20,
                    vsync_pulse_width: 4,
                    vsync_front_porch: 20,
                },
            },
            Self::St7123 => DsiPanelConfig {
                lane_bit_rate_mbps: 1_000,
                dpi_clock_mhz: 70,
                timing: DsiVideoTiming {
                    width: WIDTH,
                    height: HEIGHT,
                    hsync_back_porch: 40,
                    hsync_pulse_width: 2,
                    hsync_front_porch: 40,
                    vsync_back_porch: 8,
                    vsync_pulse_width: 2,
                    vsync_front_porch: 220,
                },
            },
            Self::St7121 => DsiPanelConfig {
                lane_bit_rate_mbps: 965,
                dpi_clock_mhz: 70,
                timing: DsiVideoTiming {
                    width: WIDTH,
                    height: HEIGHT,
                    hsync_back_porch: 40,
                    hsync_pulse_width: 2,
                    hsync_front_porch: 40,
                    vsync_back_porch: 24,
                    vsync_pulse_width: 20,
                    vsync_front_porch: 200,
                },
            },
        }
    }

    fn write_init_commands<HOST: DsiHost, DELAY: DelayNs>(
        self,
        host: &mut HOST,
        delay: &mut DELAY,
    ) -> Result<(), HOST::Error> {
        match self {
            Self::Ili9881c => {
                for command in ILI9881C_COMMANDS {
                    host.write_command(command.command, command.data)?;
                    if command.delay_ms != 0 {
                        delay.delay_ms(u32::from(command.delay_ms));
                    }
                }
            }
            Self::St7123 => {
                for command in ST7123_INIT_COMMANDS {
                    host.write_command(command.command, command.data)?;
                    if command.delay_ms != 0 {
                        delay.delay_ms(u32::from(command.delay_ms));
                    }
                }
            }
            Self::St7121 => {
                for command in ST7121_INIT_COMMANDS {
                    host.write_command(command.command, command.data)?;
                    if command.delay_ms != 0 {
                        delay.delay_ms(u32::from(command.delay_ms));
                    }
                }
            }
        }
        Ok(())
    }
}

fn detect_panel<I2C: I2c>(
    i2c: &mut I2C,
    gt911_address: u8,
    st712x_address: u8,
) -> Result<PanelRevision, DetectError<I2C::Error>> {
    if let Ok(model) = St712x::probe_model(i2c, st712x_address) {
        return match model {
            Some(St712xModel::St7121) => Ok(PanelRevision::St7121),
            Some(St712xModel::St7123) => Ok(PanelRevision::St7123),
            None => Err(DetectError::UnsupportedPanel),
        };
    }

    let gt911 = Gt911::new(gt911_address).ok_or(DetectError::UnsupportedPanel)?;
    if gt911.is_present(i2c).map_err(DetectError::Bus)? {
        Ok(PanelRevision::Ili9881c)
    } else {
        Err(DetectError::UnsupportedPanel)
    }
}

fn rgb565(color: Rgb888) -> u16 {
    (u16::from(color.r() & 0xf8) << 8)
        | (u16::from(color.g() & 0xfc) << 3)
        | (u16::from(color.b()) >> 3)
}

impl<HOST: DsiHost> OriginDimensions for Tab5DsiDisplay<HOST> {
    fn size(&self) -> Size {
        Size::new(u32::from(WIDTH), u32::from(HEIGHT))
    }
}

impl<HOST: DsiHost> DrawTarget for Tab5DsiDisplay<HOST> {
    type Color = Rgb565;
    type Error = Tab5DsiDisplayError<HOST::Error>;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if point.x < 0
                || point.y < 0
                || point.x >= i32::from(WIDTH)
                || point.y >= i32::from(HEIGHT)
            {
                continue;
            }
            let storage =
                (u16::from(color.r()) << 11) | (u16::from(color.g()) << 5) | u16::from(color.b());
            self.host
                .draw_rgb565(point.x as u16, point.y as u16, 1, 1, &[storage])
                .map_err(Tab5DsiDisplayError::Host)?;
        }
        Ok(())
    }
}

impl<HOST: DsiHost> Display for Tab5DsiDisplay<HOST> {
    type ControlError = Tab5DsiDisplayError<HOST::Error>;
    type RenderError = Tab5DsiDisplayError<HOST::Error>;

    fn descriptor(&self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Rgb565,
            DisplayTechnology::Lcd,
            DisplayFeatures::BRIGHTNESS | DisplayFeatures::SLEEP,
        )
    }

    fn draw_rgb888(&mut self, area: Rectangle, pixels: &[Rgb888]) -> Result<(), Self::RenderError> {
        let clipped = area.intersection(&self.bounding_box());
        if clipped != area || area.size.width == 0 || area.size.height == 0 {
            return Err(Tab5DsiDisplayError::InvalidPixelCount);
        }
        let expected = usize::try_from(area.size.width)
            .unwrap_or(usize::MAX)
            .saturating_mul(usize::try_from(area.size.height).unwrap_or(usize::MAX));
        if expected != pixels.len() {
            return Err(Tab5DsiDisplayError::InvalidPixelCount);
        }

        let mut row = [0u16; WIDTH as usize];
        let width = area.size.width as usize;
        for (line, source) in pixels.chunks_exact(width).enumerate() {
            for (target, color) in row[..width].iter_mut().zip(source) {
                *target = rgb565(*color);
            }
            self.host
                .draw_rgb565(
                    area.top_left.x as u16,
                    area.top_left.y as u16 + line as u16,
                    area.size.width as u16,
                    1,
                    &row[..width],
                )
                .map_err(Tab5DsiDisplayError::Host)?;
        }
        Ok(())
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        Ok(())
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        match power {
            DisplayPower::On => {
                self.host
                    .set_sleep(false)
                    .map_err(Tab5DsiDisplayError::Host)?;
                self.host
                    .set_display_enabled(true)
                    .map_err(Tab5DsiDisplayError::Host)
            }
            DisplayPower::Sleep => self.host.set_sleep(true).map_err(Tab5DsiDisplayError::Host),
            DisplayPower::Off => {
                self.host
                    .set_backlight(0)
                    .map_err(Tab5DsiDisplayError::Host)?;
                self.host
                    .set_display_enabled(false)
                    .map_err(Tab5DsiDisplayError::Host)
            }
        }
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        self.host
            .set_backlight(brightness)
            .map_err(Tab5DsiDisplayError::Host)
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        if orientation == DisplayOrientation::Deg0 {
            Ok(())
        } else {
            Err(Tab5DsiDisplayError::UnsupportedOrientation)
        }
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(())
    }
}

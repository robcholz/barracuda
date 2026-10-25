//! Display implementation for the M5Stack Paper Mono.
//!
//! The Paper Mono carries a 3.97-inch 800 x 480 GDEQ0426T82 panel (Solomon
//! SSD1677) mounted as an upright 480 x 800 portrait, with a front light.
//! Everything follows M5Stack's M5GFX (`board_M5PaperMono`, `Panel_SSD1677_4Gray`,
//! `Light_M5PaperMono`):
//!
//! - the M5PM1 power controller keeps its I2C awake, stops its watchdog, and
//!   enables its DC/DC and LDO rails, and drives the front light from PWM 0 on
//!   its G3 at 5 kHz with a squared duty;
//! - the M5IOE1 expander enables the panel (IO3) and pulses its reset (IO5);
//! - the SSD1677 shows four gray levels from its two RAM planes and a
//!   register waveform: "Mode 1" absolute updates with the quality or text
//!   waveform, and "Mode 2" absolute updates with the fast waveform. Mode 2
//!   activations swap the controller's RAM face, and Mode 1 maps the middle
//!   grays through it, so the face is tracked and the planes are swapped on
//!   an odd face as M5GFX does.
//!
//! Every update redraws the whole panel; M5GFX's differential monochrome
//! "fastest" mode is not implemented.

#![no_std]

extern crate alloc;

use alloc::{vec, vec::Vec};
use core::{convert::Infallible, marker::PhantomData};

use barracuda_driver_m5ioe1::{self as m5ioe1, M5ioe1};
use barracuda_driver_m5pm1::{self as m5pm1, M5pm1, PwmChannel, Rail};
use barracuda_driver_ssd1677::{
    self as ssd1677, CONTROL_1_NORMAL, CONTROL_2_POWER_ON, PanelGeometry, Ram, Ssd1677,
    WAVEFORM_FAST, WAVEFORM_QUALITY, WAVEFORM_TEXT, Waveform,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    display::{
        Display, DisplayDescriptor, DisplayFeatures, DisplayOrientation, DisplayPower,
        DisplayTechnology, PixelFormat, RefreshMode, RefreshRequest,
    },
};
use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Point, Size},
    pixelcolor::{Gray2, GrayColor, Rgb565, RgbColor},
    primitives::Rectangle,
};
use embedded_hal::{
    delay::DelayNs,
    digital::{InputPin, OutputPin},
    i2c::I2c,
    spi::SpiDevice,
};

/// Panel sources (native X).
const SOURCES: u16 = 800;
/// Panel gates (native Y).
const GATES: u16 = 480;
const ROW_BYTES: usize = SOURCES as usize / 8;
const PLANE_BYTES: usize = ROW_BYTES * GATES as usize;

/// Stable descriptor: upright 480 x 800, four gray levels, front light.
pub const DESCRIPTOR: DisplayDescriptor = DisplayDescriptor::new(
    Size::new(GATES as u32, SOURCES as u32),
    PixelFormat::Gray2,
    DisplayTechnology::Epaper,
    DisplayFeatures::BUFFERED
        .union(DisplayFeatures::BRIGHTNESS)
        .union(DisplayFeatures::BUSY)
        .union(DisplayFeatures::SLEEP),
);

/// M5IOE1 pin enabling the panel supply.
const IOE_PANEL_ENABLE: u8 = 3;
/// M5IOE1 pin driving the panel reset.
const IOE_PANEL_RESET: u8 = 5;
/// M5PM1 PWM channel of the front light, on G3.
const PM1_FRONT_LIGHT: PwmChannel = PwmChannel::Ch0;
const FRONT_LIGHT_HZ: u16 = 5_000;
/// Reset low and recovery times (M5GFX: 8 ms and 2 ms at bring-up).
const RESET_LOW_MS: u32 = 8;
const RESET_RECOVERY_MS: u32 = 2;
/// Display-update control 2 of a Mode 1 and a Mode 2 activation.
const CONTROL_2_MODE_1: u8 = 0x07;
const CONTROL_2_MODE_2: u8 = 0x0C;

/// M5GFX's alternative signed 4 x 4 Bayer offsets (`bayer_4x4_alt_signed`).
const BAYER_ALT_SIGNED: [i16; 16] = [
    -30, 18, -22, 26, -14, 2, -6, 10, -18, 30, -26, 22, -2, 14, -10, 6,
];

/// I2C addresses of the helper chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaperMonoConfig {
    m5pm1: u8,
    m5ioe1: u8,
}

impl PaperMonoConfig {
    /// Selects the M5PM1 and M5IOE1 addresses; out-of-range values fall back
    /// to the factory addresses `0x6E` and `0x4F`.
    #[must_use]
    pub fn new(m5pm1: i64, m5ioe1: i64) -> Self {
        Self {
            m5pm1: u8::try_from(m5pm1).unwrap_or(m5pm1::DEFAULT_ADDRESS),
            m5ioe1: u8::try_from(m5ioe1).unwrap_or(m5ioe1::DEFAULT_ADDRESS),
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct PaperMonoBindings<SPI, DC, BUSY, I2C, DELAY> {
    spi: SPI,
    dc: DC,
    busy: BUSY,
    i2c: I2C,
    delay: DELAY,
}

impl<SPI, DC, BUSY, I2C, DELAY> PaperMonoBindings<SPI, DC, BUSY, I2C, DELAY> {
    /// Combines the panel's SPI device and pins with the internal I2C bus.
    #[must_use]
    pub const fn new(spi: SPI, dc: DC, busy: BUSY, i2c: I2C, delay: DELAY) -> Self {
        Self {
            spi,
            dc,
            busy,
            i2c,
            delay,
        }
    }
}

/// Initialization or display-control failure.
#[derive(Debug)]
pub enum PaperMonoError<SPI, DC, BUSY, I2C> {
    /// The SSD1677 transaction failed or timed out.
    Panel(ssd1677::Error<SPI, DC, BUSY>),
    /// The M5PM1 rejected a register access.
    PowerController(m5pm1::Error<I2C>),
    /// The M5IOE1 rejected a register access.
    Expander(m5ioe1::Error<I2C>),
    /// Only the Board's upright orientation is supported.
    OrientationUnsupported,
}

impl<SPI, DC, BUSY, I2C> From<ssd1677::Error<SPI, DC, BUSY>>
    for PaperMonoError<SPI, DC, BUSY, I2C>
{
    fn from(error: ssd1677::Error<SPI, DC, BUSY>) -> Self {
        Self::Panel(error)
    }
}

impl<SPI, DC, BUSY, I2C> From<m5pm1::Error<I2C>> for PaperMonoError<SPI, DC, BUSY, I2C> {
    fn from(error: m5pm1::Error<I2C>) -> Self {
        Self::PowerController(error)
    }
}

impl<SPI, DC, BUSY, I2C> From<m5ioe1::Error<I2C>> for PaperMonoError<SPI, DC, BUSY, I2C> {
    fn from(error: m5ioe1::Error<I2C>) -> Self {
        Self::Expander(error)
    }
}

/// Error type of one concrete display.
pub type ErrorOf<SPI, DC, BUSY, I2C> = PaperMonoError<
    <SPI as embedded_hal::spi::ErrorType>::Error,
    <DC as embedded_hal::digital::ErrorType>::Error,
    <BUSY as embedded_hal::digital::ErrorType>::Error,
    <I2C as embedded_hal::i2c::ErrorType>::Error,
>;

/// Initialized Paper Mono display.
pub struct PaperMonoDisplay<SPI, DC, BUSY, I2C, DELAY> {
    panel: Ssd1677<SPI, DC, BUSY>,
    i2c: I2C,
    delay: DELAY,
    config: PaperMonoConfig,
    /// Low gray bit of each native pixel, MSB first, `1` lighter.
    low_plane: Vec<u8>,
    /// High gray bit of each native pixel.
    high_plane: Vec<u8>,
    /// Whether the controller's analog supply is still on.
    analog_on: bool,
    /// The controller's RAM face: `Some(odd)` when known.
    face_odd: Option<bool>,
    asleep: bool,
}

/// Static implementation factory.
pub struct PaperMonoDisplayImplementation<SPI, DC, BUSY, I2C, DELAY>(
    PhantomData<(SPI, DC, BUSY, I2C, DELAY)>,
);

impl<SPI, DC, BUSY, I2C, DELAY> PeripheralImplementation
    for PaperMonoDisplayImplementation<SPI, DC, BUSY, I2C, DELAY>
where
    SPI: SpiDevice + 'static,
    DC: OutputPin + 'static,
    BUSY: InputPin + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
    SPI::Error: 'static,
    DC::Error: 'static,
    BUSY::Error: 'static,
    I2C::Error: 'static,
{
    type Bindings = PaperMonoBindings<SPI, DC, BUSY, I2C, DELAY>;
    type Config = PaperMonoConfig;
    type Peripheral = PaperMonoDisplay<SPI, DC, BUSY, I2C, DELAY>;
    type Error = ErrorOf<SPI, DC, BUSY, I2C>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let PaperMonoBindings {
            spi,
            dc,
            busy,
            i2c,
            delay,
        } = bindings;
        let mut display = PaperMonoDisplay {
            panel: Ssd1677::new(spi, dc, busy),
            i2c,
            delay,
            config,
            low_plane: vec![0xFF; PLANE_BYTES],
            high_plane: vec![0xFF; PLANE_BYTES],
            analog_on: false,
            face_odd: None,
            asleep: true,
        };
        {
            let mut pm1 = M5pm1::new(&mut display.i2c, config.m5pm1)?;
            pm1.disable_i2c_sleep()?;
            pm1.disable_watchdog()?;
            pm1.set_rail_enabled(Rail::DcDc, true)?;
            pm1.set_rail_enabled(Rail::Ldo, true)?;
            pm1.configure_pwm_output(PM1_FRONT_LIGHT)?;
            pm1.set_pwm_frequency(FRONT_LIGHT_HZ)?;
            pm1.stop_pwm(PM1_FRONT_LIGHT)?;
        }
        {
            let mut ioe = M5ioe1::new(&mut display.i2c, config.m5ioe1)?;
            ioe.wake()?;
            ioe.configure_output(IOE_PANEL_RESET, true)?;
        }
        display.wake()?;
        Ok(display)
    }
}

impl<SPI, DC, BUSY, I2C, DELAY> PaperMonoDisplay<SPI, DC, BUSY, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    BUSY: InputPin,
    I2C: I2c,
    DELAY: DelayNs,
{
    /// Powers the panel, pulses its reset, and initializes the controller.
    fn wake(&mut self) -> Result<(), ErrorOf<SPI, DC, BUSY, I2C>> {
        {
            let mut ioe = M5ioe1::new(&mut self.i2c, self.config.m5ioe1)?;
            ioe.configure_output(IOE_PANEL_ENABLE, true)?;
            ioe.set_output(IOE_PANEL_RESET, false)?;
            self.delay.delay_ms(RESET_LOW_MS);
            ioe.set_output(IOE_PANEL_RESET, true)?;
            self.delay.delay_ms(RESET_RECOVERY_MS);
        }
        self.reset_controller()?;
        self.asleep = false;
        Ok(())
    }

    /// Software reset and initialization; establishes the even RAM face.
    fn reset_controller(&mut self) -> Result<(), ErrorOf<SPI, DC, BUSY, I2C>> {
        self.panel.initialize(
            &mut self.delay,
            PanelGeometry {
                sources: SOURCES,
                gates: GATES,
            },
        )?;
        self.analog_on = false;
        self.face_odd = Some(false);
        Ok(())
    }

    fn set_native(&mut self, x: u16, y: u16, level: u8) {
        if x >= SOURCES || y >= GATES {
            return;
        }
        let index = usize::from(y) * ROW_BYTES + usize::from(x / 8);
        let mask = 0x80_u8 >> (x % 8);
        for (plane, bit) in [
            (&mut self.low_plane, level & 1),
            (&mut self.high_plane, level & 2),
        ] {
            if let Some(byte) = plane.get_mut(index) {
                if bit == 0 {
                    *byte &= !mask;
                } else {
                    *byte |= mask;
                }
            }
        }
    }

    fn native(point: Point) -> Option<(u16, u16)> {
        let x = u16::try_from(point.x).ok()?;
        let y = u16::try_from(point.y).ok()?;
        (x < GATES && y < SOURCES).then(|| upright_to_native(x, y))
    }

    fn send_plane(&mut self, ram: Ram, high: bool) -> Result<(), ErrorOf<SPI, DC, BUSY, I2C>> {
        self.panel.set_ram_window(0, SOURCES - 1, GATES - 1, 0)?;
        self.panel.begin_ram_write(ram)?;
        let plane = if high {
            &self.high_plane
        } else {
            &self.low_plane
        };
        let mut row = [0_u8; ROW_BYTES];
        for source in plane.as_chunks::<ROW_BYTES>().0 {
            // The waveform groups count from white, the planes from black.
            for (target, byte) in row.iter_mut().zip(source) {
                *target = !*byte;
            }
            self.panel.write_data(&row)?;
        }
        Ok(())
    }

    fn activate(
        &mut self,
        waveform: &Waveform,
        control_2: u8,
    ) -> Result<(), ErrorOf<SPI, DC, BUSY, I2C>> {
        self.panel.load_waveform(waveform)?;
        let control_2 = if self.analog_on {
            control_2
        } else {
            control_2 | CONTROL_2_POWER_ON
        };
        self.panel.activate(CONTROL_1_NORMAL, control_2)?;
        self.panel
            .wait_until_idle(&mut self.delay, ssd1677::REFRESH_TIMEOUT_MS)?;
        Ok(())
    }

    fn ensure_known_face(&mut self) -> Result<bool, ErrorOf<SPI, DC, BUSY, I2C>> {
        if let Some(odd) = self.face_odd {
            return Ok(odd);
        }
        self.reset_controller()?;
        Ok(false)
    }

    /// M5GFX `_refresh_mode1_absolute`.
    fn refresh_mode_1(&mut self, waveform: &Waveform) -> Result<(), ErrorOf<SPI, DC, BUSY, I2C>> {
        let odd = self.ensure_known_face()?;
        // An odd face exchanges the two middle codes; swap the destinations.
        self.send_plane(Ram::Bw, odd)?;
        self.send_plane(Ram::Red, !odd)?;
        self.activate(waveform, CONTROL_2_MODE_1)?;
        // Mode 1 powers the analog supply down and returns to the even face.
        self.analog_on = false;
        self.face_odd = Some(false);
        Ok(())
    }

    /// M5GFX `_refresh_mode2_absolute`.
    fn refresh_mode_2(&mut self) -> Result<(), ErrorOf<SPI, DC, BUSY, I2C>> {
        let odd = self.ensure_known_face()?;
        self.send_plane(Ram::Bw, false)?;
        self.send_plane(Ram::Red, true)?;
        self.activate(&WAVEFORM_FAST, CONTROL_2_MODE_2)?;
        self.analog_on = true;
        self.face_odd = Some(!odd);
        Ok(())
    }
}

/// Maps an upright coordinate (480 x 800) to the panel's source and gate
/// (800 x 480), as M5GFX `_rotate_pos` does for `offset_rotation` 3.
#[must_use]
pub const fn upright_to_native(x: u16, y: u16) -> (u16, u16) {
    (y, GATES - 1 - x)
}

/// M5GFX's four-level reduction of one RGB565 pixel at native `(x, y)`:
/// `((R + 2G + B) / 4 + bayer) >> 6`, `0` black to `3` white.
#[must_use]
pub fn dithered_level(pixel: Rgb565, x: u16, y: u16) -> u8 {
    let expand = |value: u8, max: u16| i16::try_from(u16::from(value) * 255 / max).unwrap_or(255);
    let gray = (expand(pixel.r(), 31) + 2 * expand(pixel.g(), 63) + expand(pixel.b(), 31)) / 4;
    let index = usize::from(x & 3) | usize::from(y & 3) << 2;
    let level = (gray + BAYER_ALT_SIGNED[index]).max(0) >> 6;
    u8::try_from(level.min(3)).unwrap_or(3)
}

impl<SPI, DC, BUSY, I2C, DELAY> OriginDimensions for PaperMonoDisplay<SPI, DC, BUSY, I2C, DELAY> {
    fn size(&self) -> Size {
        DESCRIPTOR.size()
    }
}

impl<SPI, DC, BUSY, I2C, DELAY> DrawTarget for PaperMonoDisplay<SPI, DC, BUSY, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    BUSY: InputPin,
    I2C: I2c,
    DELAY: DelayNs,
{
    type Color = Gray2;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if let Some((x, y)) = Self::native(point) {
                self.set_native(x, y, color.luma());
            }
        }
        Ok(())
    }
}

impl<SPI, DC, BUSY, I2C, DELAY> Display for PaperMonoDisplay<SPI, DC, BUSY, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    BUSY: InputPin,
    I2C: I2c,
    DELAY: DelayNs,
{
    type ControlError = ErrorOf<SPI, DC, BUSY, I2C>;
    type RenderError = Infallible;

    fn descriptor(&self) -> DisplayDescriptor {
        DESCRIPTOR
    }

    fn draw_rgb565(&mut self, area: Rectangle, pixels: &[Rgb565]) -> Result<(), Self::RenderError> {
        let width = i32::try_from(area.size.width).unwrap_or(0);
        if width == 0 {
            return Ok(());
        }
        let mut offset = 0_i32;
        for pixel in pixels {
            let point = area.top_left + Point::new(offset % width, offset / width);
            if let Some((x, y)) = Self::native(point) {
                self.set_native(x, y, dithered_level(*pixel, x, y));
            }
            offset = offset.saturating_add(1);
        }
        Ok(())
    }

    async fn flush(&mut self, request: RefreshRequest) -> Result<(), Self::ControlError> {
        if self.asleep {
            self.wake()?;
        }
        // M5GFX: quality and text use Mode 1, fast uses Mode 2.
        match request.mode() {
            RefreshMode::Automatic | RefreshMode::Full => self.refresh_mode_1(&WAVEFORM_QUALITY),
            RefreshMode::Partial => self.refresh_mode_1(&WAVEFORM_TEXT),
            RefreshMode::Fast => self.refresh_mode_2(),
        }
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        match power {
            DisplayPower::On => {
                if self.asleep {
                    self.wake()?;
                }
            }
            DisplayPower::Sleep | DisplayPower::Off => {
                if !self.asleep {
                    self.panel.deep_sleep(&mut self.delay)?;
                    self.asleep = true;
                    self.analog_on = false;
                    self.face_odd = None;
                }
                if power == DisplayPower::Off {
                    let mut ioe = M5ioe1::new(&mut self.i2c, self.config.m5ioe1)?;
                    ioe.set_output(IOE_PANEL_ENABLE, false)?;
                }
            }
        }
        Ok(())
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        let mut pm1 = M5pm1::new(&mut self.i2c, self.config.m5pm1)?;
        if brightness == 0 {
            pm1.stop_pwm(PM1_FRONT_LIGHT)?;
        } else {
            // M5GFX: a squared 16-bit level scaled to the 12-bit duty.
            let squared = u32::from(brightness) * u32::from(brightness);
            let duty = u16::try_from(squared >> 4).unwrap_or(m5pm1::MAX_DUTY);
            pm1.set_pwm_duty(PM1_FRONT_LIGHT, duty, false, true)?;
        }
        Ok(())
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        if orientation == DisplayOrientation::Deg0 {
            Ok(())
        } else {
            Err(PaperMonoError::OrientationUnsupported)
        }
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        if self.asleep {
            return Ok(());
        }
        Ok(self
            .panel
            .wait_until_idle(&mut self.delay, ssd1677::DEFAULT_BUSY_TIMEOUT_MS)?)
    }
}

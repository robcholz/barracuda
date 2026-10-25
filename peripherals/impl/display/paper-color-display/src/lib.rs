//! Display implementation for the M5Stack PaperColor.
//!
//! The PaperColor carries a 4-inch 400 x 600 E Ink Spectra 6 panel driven by
//! an ED2208 controller. Everything follows M5Stack's M5GFX
//! (`board_M5PaperColor`, `Panel_ED2208`):
//!
//! - the M5PM1 power controller stops its watchdog, switches its G0 (panel
//!   supply) and G3 (microSD supply) to push-pull outputs driven high, keeps
//!   its I2C awake, and is given 100 ms to settle. The microSD card shares the
//!   panel's SPI lines, so it is powered with the panel as M5GFX does;
//! - the panel reset is pulsed, then the controller is initialized;
//! - each refresh converts the whole RGB565 framebuffer to the six panel
//!   colors with M5GFX's pairwise dithering, loads it, and runs the
//!   power-on, refresh, power-off sequence (10 to 20 seconds).
//!
//! Spectra 6 has no partial update, so every refresh redraws the whole panel.

#![no_std]

extern crate alloc;

use alloc::{vec, vec::Vec};
use core::{convert::Infallible, marker::PhantomData};

use barracuda_driver_ed2208::{self as ed2208, Dither, Ed2208};
use barracuda_driver_m5pm1::{self as m5pm1, M5pm1};
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
    pixelcolor::{IntoStorage, Rgb565, RgbColor, raw::RawU16},
    primitives::Rectangle,
};
use embedded_hal::{
    delay::DelayNs,
    digital::{InputPin, OutputPin},
    i2c::I2c,
    spi::SpiDevice,
};

const WIDTH: u16 = 400;
const HEIGHT: u16 = 600;
const PIXELS: usize = WIDTH as usize * HEIGHT as usize;
const ROW_BYTES: usize = WIDTH as usize / 2;

/// Stable descriptor: 400 x 600 portrait, RGB input reduced to six colors.
pub const DESCRIPTOR: DisplayDescriptor = DisplayDescriptor::new(
    Size::new(WIDTH as u32, HEIGHT as u32),
    PixelFormat::Rgb565,
    DisplayTechnology::Epaper,
    DisplayFeatures::BUFFERED
        .union(DisplayFeatures::BUSY)
        .union(DisplayFeatures::SLEEP),
);

/// M5PM1 pins switching the panel and microSD supplies.
const PM1_PANEL_SUPPLY: u8 = 0;
const PM1_CARD_SUPPLY: u8 = 3;
/// Settle time after the supplies rise (M5GFX: 100 ms).
const SUPPLY_SETTLE_MS: u32 = 100;
/// Reset pulse of M5GFX `_pin_reset`.
const RESET_LOW_MS: u32 = 2;
const RESET_RECOVERY_MS: u32 = 10;

/// I2C address of the M5PM1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaperColorConfig {
    m5pm1: u8,
}

impl PaperColorConfig {
    /// Selects the M5PM1 address; an out-of-range value falls back to the
    /// factory address `0x6E`.
    #[must_use]
    pub fn new(m5pm1: i64) -> Self {
        Self {
            m5pm1: u8::try_from(m5pm1).unwrap_or(m5pm1::DEFAULT_ADDRESS),
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct PaperColorBindings<SPI, DC, BUSY, RST, I2C, DELAY> {
    spi: SPI,
    dc: DC,
    busy: BUSY,
    reset: RST,
    i2c: I2C,
    delay: DELAY,
}

impl<SPI, DC, BUSY, RST, I2C, DELAY> PaperColorBindings<SPI, DC, BUSY, RST, I2C, DELAY> {
    /// Combines the panel's SPI device and pins with the internal I2C bus.
    #[must_use]
    pub const fn new(spi: SPI, dc: DC, busy: BUSY, reset: RST, i2c: I2C, delay: DELAY) -> Self {
        Self {
            spi,
            dc,
            busy,
            reset,
            i2c,
            delay,
        }
    }
}

/// Initialization or display-control failure.
#[derive(Debug)]
pub enum PaperColorError<SPI, DC, BUSY, RST, I2C> {
    /// The ED2208 transaction failed or timed out.
    Panel(ed2208::Error<SPI, DC, BUSY>),
    /// The reset output failed.
    Reset(RST),
    /// The M5PM1 rejected a register access.
    PowerController(m5pm1::Error<I2C>),
    /// This panel has no brightness control.
    BrightnessUnsupported,
    /// Only the Board's upright orientation is supported.
    OrientationUnsupported,
}

impl<SPI, DC, BUSY, RST, I2C> From<ed2208::Error<SPI, DC, BUSY>>
    for PaperColorError<SPI, DC, BUSY, RST, I2C>
{
    fn from(error: ed2208::Error<SPI, DC, BUSY>) -> Self {
        Self::Panel(error)
    }
}

impl<SPI, DC, BUSY, RST, I2C> From<m5pm1::Error<I2C>> for PaperColorError<SPI, DC, BUSY, RST, I2C> {
    fn from(error: m5pm1::Error<I2C>) -> Self {
        Self::PowerController(error)
    }
}

/// Error type of one concrete display.
pub type ErrorOf<SPI, DC, BUSY, RST, I2C> = PaperColorError<
    <SPI as embedded_hal::spi::ErrorType>::Error,
    <DC as embedded_hal::digital::ErrorType>::Error,
    <BUSY as embedded_hal::digital::ErrorType>::Error,
    <RST as embedded_hal::digital::ErrorType>::Error,
    <I2C as embedded_hal::i2c::ErrorType>::Error,
>;

/// Initialized PaperColor display.
pub struct PaperColorDisplay<SPI, DC, BUSY, RST, I2C, DELAY> {
    panel: Ed2208<SPI, DC, BUSY>,
    reset: RST,
    i2c: I2C,
    delay: DELAY,
    config: PaperColorConfig,
    /// Row-major RGB565 framebuffer.
    framebuffer: Vec<u16>,
    asleep: bool,
}

/// Static implementation factory.
pub struct PaperColorDisplayImplementation<SPI, DC, BUSY, RST, I2C, DELAY>(
    PhantomData<(SPI, DC, BUSY, RST, I2C, DELAY)>,
);

impl<SPI, DC, BUSY, RST, I2C, DELAY> PeripheralImplementation
    for PaperColorDisplayImplementation<SPI, DC, BUSY, RST, I2C, DELAY>
where
    SPI: SpiDevice + 'static,
    DC: OutputPin + 'static,
    BUSY: InputPin + 'static,
    RST: OutputPin + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
    SPI::Error: 'static,
    DC::Error: 'static,
    BUSY::Error: 'static,
    RST::Error: 'static,
    I2C::Error: 'static,
{
    type Bindings = PaperColorBindings<SPI, DC, BUSY, RST, I2C, DELAY>;
    type Config = PaperColorConfig;
    type Peripheral = PaperColorDisplay<SPI, DC, BUSY, RST, I2C, DELAY>;
    type Error = ErrorOf<SPI, DC, BUSY, RST, I2C>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let PaperColorBindings {
            spi,
            dc,
            busy,
            reset,
            i2c,
            delay,
        } = bindings;
        let mut display = PaperColorDisplay {
            panel: Ed2208::new(spi, dc, busy),
            reset,
            i2c,
            delay,
            config,
            framebuffer: vec![Rgb565::WHITE.into_storage(); PIXELS],
            asleep: true,
        };
        display.wake()?;
        Ok(display)
    }
}

impl<SPI, DC, BUSY, RST, I2C, DELAY> PaperColorDisplay<SPI, DC, BUSY, RST, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    BUSY: InputPin,
    RST: OutputPin,
    I2C: I2c,
    DELAY: DelayNs,
{
    /// Powers the panel, pulses its reset, and initializes the controller.
    fn wake(&mut self) -> Result<(), ErrorOf<SPI, DC, BUSY, RST, I2C>> {
        {
            let mut pm1 = M5pm1::new(&mut self.i2c, self.config.m5pm1)?;
            pm1.disable_watchdog()?;
            for pin in [PM1_PANEL_SUPPLY, PM1_CARD_SUPPLY] {
                pm1.configure_output(pin, true)?;
            }
            pm1.disable_i2c_sleep()?;
        }
        self.delay.delay_ms(SUPPLY_SETTLE_MS);
        self.reset.set_high().map_err(PaperColorError::Reset)?;
        self.delay.delay_ms(1);
        self.reset.set_low().map_err(PaperColorError::Reset)?;
        self.delay.delay_ms(RESET_LOW_MS);
        self.reset.set_high().map_err(PaperColorError::Reset)?;
        self.delay.delay_ms(RESET_RECOVERY_MS);
        self.panel.initialize(&mut self.delay, WIDTH, HEIGHT)?;
        self.asleep = false;
        Ok(())
    }

    fn set_pixel(&mut self, point: Point, color: Rgb565) {
        let (Ok(x), Ok(y)) = (u16::try_from(point.x), u16::try_from(point.y)) else {
            return;
        };
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        if let Some(pixel) = self
            .framebuffer
            .get_mut(usize::from(y) * usize::from(WIDTH) + usize::from(x))
        {
            *pixel = color.into_storage();
        }
    }
}

/// Expands one RGB565 pixel to 8-bit channels as M5GFX does
/// (`(c << 3) | (c >> 2)` for five bits).
#[must_use]
pub fn expand(raw: u16) -> [u8; 3] {
    let color = Rgb565::from(RawU16::new(raw));
    let five = |value: u8| value << 3 | value >> 2;
    let six = |value: u8| value << 2 | value >> 4;
    [five(color.r()), six(color.g()), five(color.b())]
}

/// M5GFX's dithering for each refresh mode: quality, text, fast.
#[must_use]
pub const fn dither_for(mode: RefreshMode) -> Dither {
    match mode {
        RefreshMode::Automatic | RefreshMode::Full => Dither::Rgb(140),
        RefreshMode::Partial => Dither::Rgb(70),
        RefreshMode::Fast => Dither::Bayer,
    }
}

impl<SPI, DC, BUSY, RST, I2C, DELAY> OriginDimensions
    for PaperColorDisplay<SPI, DC, BUSY, RST, I2C, DELAY>
{
    fn size(&self) -> Size {
        DESCRIPTOR.size()
    }
}

impl<SPI, DC, BUSY, RST, I2C, DELAY> DrawTarget
    for PaperColorDisplay<SPI, DC, BUSY, RST, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    BUSY: InputPin,
    RST: OutputPin,
    I2C: I2c,
    DELAY: DelayNs,
{
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            self.set_pixel(point, color);
        }
        Ok(())
    }
}

impl<SPI, DC, BUSY, RST, I2C, DELAY> Display for PaperColorDisplay<SPI, DC, BUSY, RST, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    BUSY: InputPin,
    RST: OutputPin,
    I2C: I2c,
    DELAY: DelayNs,
{
    type ControlError = ErrorOf<SPI, DC, BUSY, RST, I2C>;
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
            self.set_pixel(
                area.top_left + Point::new(offset % width, offset / width),
                *pixel,
            );
            offset = offset.saturating_add(1);
        }
        Ok(())
    }

    async fn flush(&mut self, request: RefreshRequest) -> Result<(), Self::ControlError> {
        if self.asleep {
            self.wake()?;
        }
        let dither = dither_for(request.mode());
        self.panel
            .wait_until_idle(&mut self.delay, ed2208::DEFAULT_BUSY_TIMEOUT_MS)?;
        self.panel.begin_frame()?;
        let mut rgb = [[0_u8; 3]; WIDTH as usize];
        let mut packed = [0_u8; ROW_BYTES];
        for (y, row) in self
            .framebuffer
            .chunks_exact(usize::from(WIDTH))
            .enumerate()
        {
            for (target, raw) in rgb.iter_mut().zip(row) {
                *target = expand(*raw);
            }
            ed2208::dither_row(&rgb, &mut packed, y, dither);
            self.panel.write_data(&packed)?;
        }
        self.panel.refresh(&mut self.delay)?;
        Ok(())
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
                    self.panel
                        .wait_until_idle(&mut self.delay, ed2208::DEFAULT_BUSY_TIMEOUT_MS)?;
                    self.panel.deep_sleep()?;
                    self.asleep = true;
                }
                if power == DisplayPower::Off {
                    let mut pm1 = M5pm1::new(&mut self.i2c, self.config.m5pm1)?;
                    pm1.set_output(PM1_PANEL_SUPPLY, false)?;
                }
            }
        }
        Ok(())
    }

    async fn set_brightness(&mut self, _brightness: u8) -> Result<(), Self::ControlError> {
        Err(PaperColorError::BrightnessUnsupported)
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        if orientation == DisplayOrientation::Deg0 {
            Ok(())
        } else {
            Err(PaperColorError::OrientationUnsupported)
        }
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        if self.asleep {
            return Ok(());
        }
        Ok(self
            .panel
            .wait_until_idle(&mut self.delay, ed2208::DEFAULT_BUSY_TIMEOUT_MS)?)
    }
}

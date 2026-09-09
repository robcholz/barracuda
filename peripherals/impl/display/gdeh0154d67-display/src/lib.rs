//! GDEH0154D67 200x200 e-paper implementation implementing Barracuda's stable display
//! peripheral.

#![no_std]

use core::{convert::Infallible, marker::PhantomData};

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
    geometry::{OriginDimensions, Size},
    pixelcolor::{Rgb888, RgbColor},
    primitives::Rectangle,
};
use embedded_hal::{
    delay::DelayNs,
    digital::{InputPin, OutputPin},
    spi::SpiDevice,
};
use epd_waveshare::{
    color::Color,
    epd1in54_v2::{Display1in54, Epd1in54, HEIGHT, WIDTH},
    prelude::{RefreshLut, WaveshareDisplay},
};

/// Stable descriptor shared by every GDEH0154D67 instance.
pub const DESCRIPTOR: DisplayDescriptor = DisplayDescriptor::new(
    Size::new(WIDTH, HEIGHT),
    PixelFormat::Binary,
    DisplayTechnology::Epaper,
    DisplayFeatures::BUFFERED
        .union(DisplayFeatures::BUSY)
        .union(DisplayFeatures::SLEEP),
);

/// Move-only resources consumed by the GDEH0154D67 implementation.
pub struct Gdeh0154d67Bindings<SPI, BUSY, DC, RST, POWER, DELAY> {
    spi: SPI,
    busy: BUSY,
    dc: DC,
    reset: RST,
    power_hold: POWER,
    delay: DELAY,
}

impl<SPI, BUSY, DC, RST, POWER, DELAY> Gdeh0154d67Bindings<SPI, BUSY, DC, RST, POWER, DELAY> {
    /// Combines the selected SPI device and panel control signals.
    #[must_use]
    pub const fn new(
        spi: SPI,
        busy: BUSY,
        dc: DC,
        reset: RST,
        power_hold: POWER,
        delay: DELAY,
    ) -> Self {
        Self {
            spi,
            busy,
            dc,
            reset,
            power_hold,
            delay,
        }
    }
}

/// Initialization failure from the panel transport or power-hold output.
#[derive(Debug)]
pub enum Gdeh0154d67InitError<SPI, POWER> {
    /// Panel SPI transaction failed.
    Spi(SPI),
    /// Board power-hold output rejected its active state.
    Power(POWER),
}

/// Stable display-control failure.
#[derive(Debug)]
pub enum Gdeh0154d67ControlError<SPI, POWER> {
    /// Panel SPI transaction failed.
    Spi(SPI),
    /// Board power-hold output rejected the requested state.
    Power(POWER),
    /// This panel has no brightness control.
    BrightnessUnsupported,
    /// This fixed panel does not rotate in hardware.
    OrientationUnsupported,
}

/// Initialized buffered GDEH0154D67 display.
pub struct Gdeh0154d67Display<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DC: OutputPin,
    RST: OutputPin,
    POWER: OutputPin,
    DELAY: DelayNs,
{
    epd: Epd1in54<SPI, BUSY, DC, RST, DELAY>,
    spi: SPI,
    power_hold: POWER,
    delay: DELAY,
    framebuffer: Display1in54,
}

/// Static GDEH0154D67 implementation factory.
pub struct Gdeh0154d67DisplayImplementation<SPI, BUSY, DC, RST, POWER, DELAY>(
    PhantomData<SPI>,
    PhantomData<BUSY>,
    PhantomData<DC>,
    PhantomData<RST>,
    PhantomData<POWER>,
    PhantomData<DELAY>,
);

impl<SPI, BUSY, DC, RST, POWER, DELAY> PeripheralImplementation
    for Gdeh0154d67DisplayImplementation<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice + 'static,
    BUSY: InputPin + 'static,
    DC: OutputPin + 'static,
    RST: OutputPin + 'static,
    POWER: OutputPin + 'static,
    DELAY: DelayNs + 'static,
    SPI::Error: 'static,
    POWER::Error: 'static,
{
    type Bindings = Gdeh0154d67Bindings<SPI, BUSY, DC, RST, POWER, DELAY>;
    type Config = ();
    type Peripheral = Gdeh0154d67Display<SPI, BUSY, DC, RST, POWER, DELAY>;
    type Error = Gdeh0154d67InitError<SPI::Error, POWER::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        (): Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let Gdeh0154d67Bindings {
            mut spi,
            busy,
            dc,
            reset,
            mut power_hold,
            mut delay,
        } = bindings;
        power_hold.set_high().map_err(Gdeh0154d67InitError::Power)?;
        let epd = Epd1in54::new(&mut spi, busy, dc, reset, &mut delay, None)
            .map_err(Gdeh0154d67InitError::Spi)?;
        Ok(Gdeh0154d67Display {
            epd,
            spi,
            power_hold,
            delay,
            framebuffer: Display1in54::default(),
        })
    }
}

impl<SPI, BUSY, DC, RST, POWER, DELAY> OriginDimensions
    for Gdeh0154d67Display<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DC: OutputPin,
    RST: OutputPin,
    POWER: OutputPin,
    DELAY: DelayNs,
{
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl<SPI, BUSY, DC, RST, POWER, DELAY> DrawTarget
    for Gdeh0154d67Display<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DC: OutputPin,
    RST: OutputPin,
    POWER: OutputPin,
    DELAY: DelayNs,
{
    type Color = Color;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        self.framebuffer.draw_iter(pixels)
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        self.framebuffer.fill_contiguous(area, colors)
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        self.framebuffer.fill_solid(area, color)
    }
}

impl<SPI, BUSY, DC, RST, POWER, DELAY> Display
    for Gdeh0154d67Display<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DC: OutputPin,
    RST: OutputPin,
    POWER: OutputPin,
    DELAY: DelayNs,
{
    type ControlError = Gdeh0154d67ControlError<SPI::Error, POWER::Error>;
    type RenderError = Infallible;

    fn descriptor(&self) -> DisplayDescriptor {
        DESCRIPTOR
    }

    fn draw_rgb888(&mut self, area: Rectangle, pixels: &[Rgb888]) -> Result<(), Self::RenderError> {
        self.fill_contiguous(
            &area,
            pixels.iter().map(|pixel| {
                if u16::from(pixel.r()) + u16::from(pixel.g()) + u16::from(pixel.b()) >= 384 {
                    Color::White
                } else {
                    Color::Black
                }
            }),
        )
    }

    async fn flush(&mut self, request: RefreshRequest) -> Result<(), Self::ControlError> {
        let lut = match request.mode() {
            RefreshMode::Partial | RefreshMode::Fast => RefreshLut::Quick,
            RefreshMode::Automatic | RefreshMode::Full => RefreshLut::Full,
        };
        self.epd
            .set_lut(&mut self.spi, &mut self.delay, Some(lut))
            .map_err(Gdeh0154d67ControlError::Spi)?;
        self.epd
            .update_and_display_frame(&mut self.spi, self.framebuffer.buffer(), &mut self.delay)
            .map_err(Gdeh0154d67ControlError::Spi)
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        match power {
            DisplayPower::On => {
                self.power_hold
                    .set_high()
                    .map_err(Gdeh0154d67ControlError::Power)?;
                self.epd
                    .wake_up(&mut self.spi, &mut self.delay)
                    .map_err(Gdeh0154d67ControlError::Spi)
            }
            DisplayPower::Sleep => self
                .epd
                .sleep(&mut self.spi, &mut self.delay)
                .map_err(Gdeh0154d67ControlError::Spi),
            DisplayPower::Off => {
                self.epd
                    .sleep(&mut self.spi, &mut self.delay)
                    .map_err(Gdeh0154d67ControlError::Spi)?;
                self.power_hold
                    .set_low()
                    .map_err(Gdeh0154d67ControlError::Power)
            }
        }
    }

    async fn set_brightness(&mut self, _brightness: u8) -> Result<(), Self::ControlError> {
        Err(Gdeh0154d67ControlError::BrightnessUnsupported)
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        if orientation == DisplayOrientation::Deg0 {
            Ok(())
        } else {
            Err(Gdeh0154d67ControlError::OrientationUnsupported)
        }
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        self.epd
            .wait_until_idle(&mut self.spi, &mut self.delay)
            .map_err(Gdeh0154d67ControlError::Spi)
    }
}

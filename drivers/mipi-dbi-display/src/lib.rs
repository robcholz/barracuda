//! MIPI DBI/DCS display Driver implementing Barracuda's stable display API.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver::{
    PeripheralDriver,
    display::{
        Display, DisplayDescriptor, DisplayFeatures, DisplayOrientation, DisplayPower,
        DisplayTechnology, PixelFormat, RefreshRequest,
    },
};
use embedded_graphics_core::{
    Pixel, draw_target::DrawTarget, geometry::OriginDimensions, pixelcolor::Rgb888,
    primitives::Rectangle,
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, spi::SpiDevice};
use mipidsi::{
    Builder, Display as MipidsiDisplay, InitError,
    interface::{Interface, InterfacePixelFormat, SpiError, SpiInterface},
    models::Model,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
};

/// Pixel dimensions used by generated portable Driver configuration.
pub use embedded_graphics_core::geometry::Size;
/// MIPI DBI controller model types available to static Board generation.
pub use mipidsi::models::{GC9A01, GC9107, ILI9342CRgb565, ST7735s, ST7789};

/// Takes the statically allocated transfer buffer used by one generated panel.
///
/// Generated Board initialization constructs its primary display once, so the
/// returned buffer has one move-only owner for the lifetime of the application.
#[must_use]
pub fn take_transfer_buffer() -> &'static mut [u8; 512] {
    static BUFFER: static_cell::StaticCell<[u8; 512]> = static_cell::StaticCell::new();
    BUFFER.init([0; 512])
}

/// Color component order selected in a panel's MADCTL register.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MipiDbiColorOrder {
    /// Red component first.
    Rgb,
    /// Blue component first, common on M5Stack panel modules.
    #[default]
    Bgr,
}

/// Portable configuration shared by all supported MIPI DBI controllers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MipiDbiDisplayConfig {
    size: Size,
    offset: (u16, u16),
    pixel_format: PixelFormat,
    orientation: DisplayOrientation,
    color_order: MipiDbiColorOrder,
    invert_colors: bool,
    backlight_active_high: Option<bool>,
}

impl MipiDbiDisplayConfig {
    /// Creates direct-panel configuration with zero framebuffer offset, BGR
    /// component order, and normal colors.
    #[must_use]
    pub const fn new(
        size: Size,
        pixel_format: PixelFormat,
        orientation: DisplayOrientation,
    ) -> Self {
        Self {
            size,
            offset: (0, 0),
            pixel_format,
            orientation,
            color_order: MipiDbiColorOrder::Bgr,
            invert_colors: false,
            backlight_active_high: None,
        }
    }

    /// Sets the controller framebuffer offset.
    #[must_use]
    pub const fn with_offset(mut self, x: u16, y: u16) -> Self {
        self.offset = (x, y);
        self
    }

    /// Sets RGB or BGR component order.
    #[must_use]
    pub const fn with_color_order(mut self, color_order: MipiDbiColorOrder) -> Self {
        self.color_order = color_order;
        self
    }

    /// Enables or disables controller color inversion.
    #[must_use]
    pub const fn with_inverted_colors(mut self, invert_colors: bool) -> Self {
        self.invert_colors = invert_colors;
        self
    }

    /// Declares a digital backlight output and its active polarity.
    #[must_use]
    pub const fn with_digital_backlight(mut self, active_high: bool) -> Self {
        self.backlight_active_high = Some(active_high);
        self
    }

    /// Returns the stable capability descriptor.
    #[must_use]
    pub fn descriptor(self) -> DisplayDescriptor {
        let mut features = DisplayFeatures::ORIENTATION | DisplayFeatures::SLEEP;
        if self.backlight_active_high.is_some() {
            features = features | DisplayFeatures::BRIGHTNESS;
        }
        let logical_size = match self.orientation {
            DisplayOrientation::Deg0 | DisplayOrientation::Deg180 => self.size,
            DisplayOrientation::Deg90 | DisplayOrientation::Deg270 => {
                Size::new(self.size.height, self.size.width)
            }
        };
        DisplayDescriptor::new(
            logical_size,
            self.pixel_format,
            DisplayTechnology::Lcd,
            features,
        )
    }
}

/// Concrete resources consumed during controller initialization.
pub struct MipiDbiDisplayBindings<SPI, DC, RST, BL, DELAY, const BUFFER_SIZE: usize> {
    spi: SPI,
    dc: DC,
    reset: RST,
    backlight: Option<BL>,
    delay: DELAY,
    buffer: &'static mut [u8; BUFFER_SIZE],
}

impl<SPI, DC, RST, BL, DELAY, const BUFFER_SIZE: usize>
    MipiDbiDisplayBindings<SPI, DC, RST, BL, DELAY, BUFFER_SIZE>
{
    /// Combines a selected SPI device, control outputs, delay source, and
    /// statically owned transfer buffer.
    #[must_use]
    pub const fn new(
        spi: SPI,
        dc: DC,
        reset: RST,
        backlight: Option<BL>,
        delay: DELAY,
        buffer: &'static mut [u8; BUFFER_SIZE],
    ) -> Self {
        Self {
            spi,
            dc,
            reset,
            backlight,
            delay,
            buffer,
        }
    }
}

/// Model and portable options used by the generic Driver factory.
pub struct MipiDbiDriverConfig<M> {
    model: M,
    display: MipiDbiDisplayConfig,
}

impl<M> MipiDbiDriverConfig<M> {
    /// Selects one `mipidsi` controller model and its Board-specific options.
    #[must_use]
    pub const fn new(model: M, display: MipiDbiDisplayConfig) -> Self {
        Self { model, display }
    }
}

/// Initialization failure or unsupported stable capability operation.
#[derive(Debug)]
pub enum MipiDbiControlError<E, B> {
    /// The transport interface rejected a controller command.
    Interface(E),
    /// The Board's backlight output rejected the requested state.
    Backlight(B),
    /// The direct panel has no standard brightness control.
    BrightnessUnsupported,
}

/// Failure while initializing the controller or enabling its backlight.
#[derive(Debug)]
pub enum MipiDbiInitError<I, R, B> {
    /// MIPI controller initialization failed.
    Display(InitError<I, R>),
    /// Backlight output rejected its initial on state.
    Backlight(B),
}

/// Initialized direct panel implementing Barracuda's stable display API.
pub struct MipiDbiDisplay<DI, M, RST, BL, DELAY>
where
    DI: Interface,
    M: Model,
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
    RST: OutputPin,
{
    inner: MipidsiDisplay<DI, M, RST>,
    delay: DELAY,
    backlight: Option<BL>,
    config: MipiDbiDisplayConfig,
}

/// Static factory for all controller models supported by `mipidsi`.
pub struct MipiDbiDisplayDriver<SPI, DC, M, RST, BL, DELAY, const BUFFER_SIZE: usize>(
    PhantomData<SPI>,
    PhantomData<DC>,
    PhantomData<M>,
    PhantomData<RST>,
    PhantomData<BL>,
    PhantomData<DELAY>,
);

impl<SPI, DC, M, RST, BL, DELAY, const BUFFER_SIZE: usize> PeripheralDriver
    for MipiDbiDisplayDriver<SPI, DC, M, RST, BL, DELAY, BUFFER_SIZE>
where
    SPI: SpiDevice + 'static,
    DC: OutputPin + 'static,
    M: Model + 'static,
    M::ColorFormat: InterfacePixelFormat<u8>,
    RST: OutputPin + 'static,
    BL: OutputPin + 'static,
    DELAY: DelayNs + 'static,
    SPI::Error: 'static,
    DC::Error: 'static,
    RST::Error: 'static,
    BL::Error: 'static,
{
    type Bindings = MipiDbiDisplayBindings<SPI, DC, RST, BL, DELAY, BUFFER_SIZE>;
    type Config = MipiDbiDriverConfig<M>;
    type Capability = MipiDbiDisplay<SpiInterface<'static, SPI, DC>, M, RST, BL, DELAY>;
    type Error = MipiDbiInitError<SpiError<SPI::Error, DC::Error>, RST::Error, BL::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Capability, Self::Error> {
        let MipiDbiDisplayBindings {
            spi,
            dc,
            reset,
            mut backlight,
            mut delay,
            buffer,
        } = bindings;
        let interface = SpiInterface::new(spi, dc, buffer);
        let size = config.display.size;
        let color_order = match config.display.color_order {
            MipiDbiColorOrder::Rgb => ColorOrder::Rgb,
            MipiDbiColorOrder::Bgr => ColorOrder::Bgr,
        };
        let inversion = if config.display.invert_colors {
            ColorInversion::Inverted
        } else {
            ColorInversion::Normal
        };
        let inner = Builder::new(config.model, interface)
            .reset_pin(reset)
            .display_size(size.width as u16, size.height as u16)
            .display_offset(config.display.offset.0, config.display.offset.1)
            .color_order(color_order)
            .invert_colors(inversion)
            .orientation(mipidsi_orientation(config.display.orientation))
            .init(&mut delay)
            .map_err(MipiDbiInitError::Display)?;
        if let (Some(backlight), Some(active_high)) =
            (&mut backlight, config.display.backlight_active_high)
        {
            set_backlight(backlight, active_high, true).map_err(MipiDbiInitError::Backlight)?;
        }
        Ok(MipiDbiDisplay {
            inner,
            delay,
            backlight,
            config: config.display,
        })
    }
}

impl<DI, M, RST, BL, DELAY> OriginDimensions for MipiDbiDisplay<DI, M, RST, BL, DELAY>
where
    DI: Interface,
    M: Model,
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
    RST: OutputPin,
{
    fn size(&self) -> Size {
        self.inner.size()
    }
}

impl<DI, M, RST, BL, DELAY> DrawTarget for MipiDbiDisplay<DI, M, RST, BL, DELAY>
where
    DI: Interface,
    M: Model,
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
    RST: OutputPin,
{
    type Color = M::ColorFormat;
    type Error = DI::Error;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        self.inner.draw_iter(pixels)
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        self.inner.fill_contiguous(area, colors)
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        self.inner.fill_solid(area, color)
    }
}

impl<DI, M, RST, BL, DELAY> Display for MipiDbiDisplay<DI, M, RST, BL, DELAY>
where
    DI: Interface,
    M: Model,
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
    M::ColorFormat: From<Rgb888>,
    RST: OutputPin,
    BL: OutputPin,
    DELAY: DelayNs,
{
    type ControlError = MipiDbiControlError<DI::Error, BL::Error>;
    type RenderError = DI::Error;

    fn descriptor(&self) -> DisplayDescriptor {
        self.config.descriptor()
    }

    fn draw_rgb888(&mut self, area: Rectangle, pixels: &[Rgb888]) -> Result<(), Self::RenderError> {
        self.fill_contiguous(&area, pixels.iter().copied().map(Into::into))
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        Ok(())
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        match power {
            DisplayPower::On => {
                self.inner
                    .wake(&mut self.delay)
                    .map_err(MipiDbiControlError::Interface)?;
                self.set_configured_backlight(true)
            }
            DisplayPower::Sleep | DisplayPower::Off => {
                self.set_configured_backlight(false)?;
                self.inner
                    .sleep(&mut self.delay)
                    .map_err(MipiDbiControlError::Interface)
            }
        }
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        let (Some(backlight), Some(active_high)) =
            (&mut self.backlight, self.config.backlight_active_high)
        else {
            return Err(MipiDbiControlError::BrightnessUnsupported);
        };
        set_backlight(backlight, active_high, brightness != 0)
            .map_err(MipiDbiControlError::Backlight)
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        self.inner
            .set_orientation(mipidsi_orientation(orientation))
            .map_err(MipiDbiControlError::Interface)?;
        self.config.orientation = orientation;
        Ok(())
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(())
    }
}

impl<DI, M, RST, BL, DELAY> MipiDbiDisplay<DI, M, RST, BL, DELAY>
where
    DI: Interface,
    M: Model,
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
    RST: OutputPin,
    BL: OutputPin,
{
    fn set_configured_backlight(
        &mut self,
        on: bool,
    ) -> Result<(), MipiDbiControlError<DI::Error, BL::Error>> {
        let (Some(backlight), Some(active_high)) =
            (&mut self.backlight, self.config.backlight_active_high)
        else {
            return Ok(());
        };
        set_backlight(backlight, active_high, on).map_err(MipiDbiControlError::Backlight)
    }
}

fn set_backlight<BL: OutputPin>(
    backlight: &mut BL,
    active_high: bool,
    on: bool,
) -> Result<(), BL::Error> {
    if active_high == on {
        backlight.set_high()
    } else {
        backlight.set_low()
    }
}

const fn mipidsi_orientation(orientation: DisplayOrientation) -> Orientation {
    let rotation = match orientation {
        DisplayOrientation::Deg0 => Rotation::Deg0,
        DisplayOrientation::Deg90 => Rotation::Deg90,
        DisplayOrientation::Deg180 => Rotation::Deg180,
        DisplayOrientation::Deg270 => Rotation::Deg270,
    };
    Orientation::new().rotate(rotation)
}

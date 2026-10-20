//! MIPI DBI/DCS display implementation implementing Barracuda's stable display API.

#![no_std]

use core::marker::PhantomData;

use barracuda_peripheral::{
    PeripheralImplementation,
    display::{
        Display, DisplayDescriptor, DisplayFeatures, DisplayOrientation, DisplayPower,
        DisplayTechnology, PixelFormat, RefreshRequest,
    },
};
use embedded_graphics_core::{
    Pixel, draw_target::DrawTarget, geometry::OriginDimensions, pixelcolor::Rgb565,
    primitives::Rectangle,
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, spi::SpiDevice};
use mipidsi::{
    Builder, Display as MipidsiDisplay, InitError,
    interface::{Interface, InterfacePixelFormat, SpiError, SpiInterface},
    models::Model,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
};

/// Pixel dimensions used by generated portable implementation configuration.
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

/// Backlight beside a MIPI DBI controller, switched or dimmed by the Board.
///
/// [`MipiDbiDisplayImplementation`] drives a digital output through
/// [`DigitalBacklight`]. An implementation whose backlight is a regulator or
/// another helper chip supplies its own control and builds the panel with
/// [`initialize_on`].
pub trait PanelBacklight {
    /// Failure reported by the backlight control.
    type Error: core::fmt::Debug;

    /// Applies a brightness level on the portable `0..=255` scale, where zero
    /// turns the backlight off.
    ///
    /// # Errors
    ///
    /// Returns the control's failure.
    fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::Error>;
}

/// Digital backlight output with a fixed active level: any non-zero
/// brightness turns it on.
pub struct DigitalBacklight<BL> {
    output: BL,
    active_high: bool,
}

impl<BL> DigitalBacklight<BL> {
    /// Wraps a backlight output that lights when driven high (`true`) or low.
    #[must_use]
    pub const fn new(output: BL, active_high: bool) -> Self {
        Self {
            output,
            active_high,
        }
    }
}

impl<BL: OutputPin> PanelBacklight for DigitalBacklight<BL> {
    type Error = BL::Error;

    fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::Error> {
        if self.active_high == (brightness != 0) {
            self.output.set_high()
        } else {
            self.output.set_low()
        }
    }
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
    reset_active_high: bool,
}

impl MipiDbiDisplayConfig {
    /// Creates direct-panel configuration with zero framebuffer offset, BGR
    /// component order, normal colors, and an active-low reset.
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
            reset_active_high: false,
        }
    }

    /// Declares the reset output's active polarity. Most panels reset while
    /// the line is low; some Boards invert it with a transistor.
    #[must_use]
    pub const fn with_reset_active_high(mut self, active_high: bool) -> Self {
        self.reset_active_high = active_high;
        self
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

    /// Returns the stable peripheral descriptor.
    #[must_use]
    pub fn descriptor(self) -> DisplayDescriptor {
        self.descriptor_with_brightness(self.backlight_active_high.is_some())
    }

    fn descriptor_with_brightness(self, brightness: bool) -> DisplayDescriptor {
        let mut features = DisplayFeatures::ORIENTATION | DisplayFeatures::SLEEP;
        if brightness {
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
pub struct MipiDbiDisplayBindings<SPI, DC, RST, BL, HOLD, DELAY, const BUFFER_SIZE: usize> {
    spi: SPI,
    dc: DC,
    reset: Option<RST>,
    backlight: Option<BL>,
    power_hold: Option<HOLD>,
    delay: DELAY,
    buffer: &'static mut [u8; BUFFER_SIZE],
}

impl<SPI, DC, RST, BL, HOLD, DELAY, const BUFFER_SIZE: usize>
    MipiDbiDisplayBindings<SPI, DC, RST, BL, HOLD, DELAY, BUFFER_SIZE>
{
    /// Combines a selected SPI device, control outputs, delay source, and
    /// statically owned transfer buffer. A panel without a reset line is
    /// reset with the DCS software-reset command instead.
    ///
    /// `power_hold` is a Board power-latch output that its binding already
    /// drives high. The display keeps it for its whole lifetime so the product
    /// stays powered on battery.
    #[must_use]
    pub const fn new(
        spi: SPI,
        dc: DC,
        reset: Option<RST>,
        backlight: Option<BL>,
        power_hold: Option<HOLD>,
        delay: DELAY,
        buffer: &'static mut [u8; BUFFER_SIZE],
    ) -> Self {
        Self {
            spi,
            dc,
            reset,
            backlight,
            power_hold,
            delay,
            buffer,
        }
    }
}

/// Model and portable options used by the generic implementation factory.
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

    /// Wraps the Board's backlight output with the polarity this
    /// configuration declares through
    /// [`MipiDbiDisplayConfig::with_digital_backlight`], or returns `None`
    /// when it declares no digital backlight or there is no output.
    #[must_use]
    pub fn digital_backlight<BL>(&self, output: Option<BL>) -> Option<DigitalBacklight<BL>> {
        let active_high = self.display.backlight_active_high?;
        output.map(|output| DigitalBacklight::new(output, active_high))
    }
}

/// Initialization failure or unsupported stable peripheral operation.
#[derive(Debug)]
pub enum MipiDbiControlError<E, B> {
    /// The transport interface rejected a controller command.
    Interface(E),
    /// The Board's backlight control rejected the requested state.
    Backlight(B),
    /// The direct panel has no standard brightness control.
    BrightnessUnsupported,
}

/// Failure while initializing the controller or enabling its backlight.
#[derive(Debug)]
pub enum MipiDbiInitError<I, R, B> {
    /// MIPI controller initialization failed.
    Display(InitError<I, R>),
    /// Backlight control rejected its initial on state.
    Backlight(B),
}

/// Initialized direct panel implementing Barracuda's stable display API.
///
/// `HOLD` is a Board resource the panel keeps alive with it, such as a
/// power-latch output, or `()` when there is none.
pub struct MipiDbiDisplay<DI, M, RST, BL, DELAY, HOLD = ()>
where
    DI: Interface,
    M: Model,
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
    RST: OutputPin,
{
    inner: MipidsiDisplay<DI, M, PanelReset<RST>>,
    delay: DELAY,
    backlight: Option<BL>,
    brightness: u8,
    config: MipiDbiDisplayConfig,
    _hold: HOLD,
}

/// Result of [`initialize_on`].
pub type InitResult<DI, M, RST, BL, DELAY, HOLD = ()> = Result<
    MipiDbiDisplay<DI, M, RST, BL, DELAY, HOLD>,
    MipiDbiInitError<
        <DI as Interface>::Error,
        <RST as embedded_hal::digital::ErrorType>::Error,
        <BL as PanelBacklight>::Error,
    >,
>;

/// Initializes a MIPI DBI controller on any `mipidsi` interface, such as an
/// SPI link or a parallel bus, and lights its backlight.
///
/// The SPI factory [`MipiDbiDisplayImplementation`] calls this with an
/// [`SpiInterface`] and a [`DigitalBacklight`]. An implementation that powers
/// the panel or releases its reset through a helper chip does that first, then
/// calls this with its own interface, [`PanelBacklight`], and any resource
/// `hold` the panel must keep alive (`()` when there is none).
///
/// `reset` is the GPIO reset line, active low unless the configuration says
/// [`MipiDbiDisplayConfig::with_reset_active_high`]. Without one, the DCS
/// software reset returns the controller to its power-on defaults first.
///
/// # Errors
///
/// Returns [`MipiDbiInitError`] when the controller or backlight fails.
pub fn initialize_on<DI, M, RST, BL, DELAY, HOLD>(
    mut interface: DI,
    reset: Option<RST>,
    backlight: Option<BL>,
    hold: HOLD,
    mut delay: DELAY,
    config: MipiDbiDriverConfig<M>,
) -> InitResult<DI, M, RST, BL, DELAY, HOLD>
where
    DI: Interface,
    M: Model,
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
    RST: OutputPin,
    BL: PanelBacklight,
    DELAY: DelayNs,
{
    if reset.is_none() {
        // Without a reset line, the DCS software reset returns the
        // controller to its power-on defaults before the model init.
        interface
            .send_command(SOFTWARE_RESET, &[])
            .map_err(|error| MipiDbiInitError::Display(InitError::Interface(error)))?;
        delay.delay_ms(SOFTWARE_RESET_SETTLE_MS);
    }
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
        .reset_pin(PanelReset::new(reset, config.display.reset_active_high))
        .display_size(size.width as u16, size.height as u16)
        .display_offset(config.display.offset.0, config.display.offset.1)
        .color_order(color_order)
        .invert_colors(inversion)
        .orientation(mipidsi_orientation(config.display.orientation))
        .init(&mut delay)
        .map_err(MipiDbiInitError::Display)?;
    let mut display = MipiDbiDisplay {
        inner,
        delay,
        backlight,
        brightness: u8::MAX,
        config: config.display,
        _hold: hold,
    };
    display
        .apply_backlight(u8::MAX)
        .map_err(MipiDbiInitError::Backlight)?;
    Ok(display)
}

/// Static factory for all controller models supported by `mipidsi`.
pub struct MipiDbiDisplayImplementation<SPI, DC, M, RST, BL, HOLD, DELAY, const BUFFER_SIZE: usize>(
    PhantomData<SPI>,
    PhantomData<DC>,
    PhantomData<M>,
    PhantomData<RST>,
    PhantomData<BL>,
    PhantomData<HOLD>,
    PhantomData<DELAY>,
);

impl<SPI, DC, M, RST, BL, HOLD, DELAY, const BUFFER_SIZE: usize> PeripheralImplementation
    for MipiDbiDisplayImplementation<SPI, DC, M, RST, BL, HOLD, DELAY, BUFFER_SIZE>
where
    SPI: SpiDevice + 'static,
    DC: OutputPin + 'static,
    M: Model + 'static,
    M::ColorFormat: InterfacePixelFormat<u8>,
    RST: OutputPin + 'static,
    BL: OutputPin + 'static,
    HOLD: 'static,
    DELAY: DelayNs + 'static,
    SPI::Error: 'static,
    DC::Error: 'static,
    RST::Error: 'static,
    BL::Error: 'static,
{
    type Bindings = MipiDbiDisplayBindings<SPI, DC, RST, BL, HOLD, DELAY, BUFFER_SIZE>;
    type Config = MipiDbiDriverConfig<M>;
    type Peripheral = MipiDbiDisplay<
        SpiInterface<'static, SPI, DC>,
        M,
        RST,
        DigitalBacklight<BL>,
        DELAY,
        Option<HOLD>,
    >;
    type Error = MipiDbiInitError<SpiError<SPI::Error, DC::Error>, RST::Error, BL::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let MipiDbiDisplayBindings {
            spi,
            dc,
            reset,
            backlight,
            power_hold,
            delay,
            buffer,
        } = bindings;
        let backlight = config.digital_backlight(backlight);
        initialize_on(
            SpiInterface::new(spi, dc, buffer),
            reset,
            backlight,
            power_hold,
            delay,
            config,
        )
    }
}

impl<DI, M, RST, BL, DELAY, HOLD> OriginDimensions for MipiDbiDisplay<DI, M, RST, BL, DELAY, HOLD>
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

impl<DI, M, RST, BL, DELAY, HOLD> DrawTarget for MipiDbiDisplay<DI, M, RST, BL, DELAY, HOLD>
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

impl<DI, M, RST, BL, DELAY, HOLD> Display for MipiDbiDisplay<DI, M, RST, BL, DELAY, HOLD>
where
    DI: Interface,
    M: Model,
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
    M::ColorFormat: From<Rgb565>,
    RST: OutputPin,
    BL: PanelBacklight,
    DELAY: DelayNs,
{
    type ControlError = MipiDbiControlError<DI::Error, BL::Error>;
    type RenderError = DI::Error;

    fn descriptor(&self) -> DisplayDescriptor {
        self.config
            .descriptor_with_brightness(self.backlight.is_some())
    }

    fn draw_rgb565(&mut self, area: Rectangle, pixels: &[Rgb565]) -> Result<(), Self::RenderError> {
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
                self.apply_backlight(self.brightness)
                    .map_err(MipiDbiControlError::Backlight)
            }
            DisplayPower::Sleep | DisplayPower::Off => {
                self.apply_backlight(0)
                    .map_err(MipiDbiControlError::Backlight)?;
                self.inner
                    .sleep(&mut self.delay)
                    .map_err(MipiDbiControlError::Interface)
            }
        }
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        let Some(backlight) = &mut self.backlight else {
            return Err(MipiDbiControlError::BrightnessUnsupported);
        };
        backlight
            .set_brightness(brightness)
            .map_err(MipiDbiControlError::Backlight)?;
        // Zero only darkens the panel; powering it on again restores the last
        // non-zero level.
        if brightness != 0 {
            self.brightness = brightness;
        }
        Ok(())
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

impl<DI, M, RST, BL, DELAY, HOLD> MipiDbiDisplay<DI, M, RST, BL, DELAY, HOLD>
where
    DI: Interface,
    M: Model,
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
    RST: OutputPin,
    BL: PanelBacklight,
{
    fn apply_backlight(&mut self, brightness: u8) -> Result<(), BL::Error> {
        match &mut self.backlight {
            Some(backlight) => backlight.set_brightness(brightness),
            None => Ok(()),
        }
    }
}

/// DCS software reset command.
const SOFTWARE_RESET: u8 = 0x01;
/// Time the MIPI DCS controllers need after a software reset before sleep-out.
const SOFTWARE_RESET_SETTLE_MS: u32 = 150;

/// The panel reset line as the active-low pin `mipidsi` pulses: an absent
/// line does nothing and an active-high line is inverted.
pub struct PanelReset<RST> {
    pin: Option<RST>,
    active_high: bool,
}

impl<RST> PanelReset<RST> {
    /// Wraps an optional reset output and its active polarity.
    #[must_use]
    pub const fn new(pin: Option<RST>, active_high: bool) -> Self {
        Self { pin, active_high }
    }
}

impl<RST: OutputPin> embedded_hal::digital::ErrorType for PanelReset<RST> {
    type Error = RST::Error;
}

impl<RST: OutputPin> OutputPin for PanelReset<RST> {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.drive(true)
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.drive(false)
    }
}

impl<RST: OutputPin> PanelReset<RST> {
    fn drive(&mut self, asserted: bool) -> Result<(), RST::Error> {
        let Some(pin) = &mut self.pin else {
            return Ok(());
        };
        if asserted == self.active_high {
            pin.set_high()
        } else {
            pin.set_low()
        }
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

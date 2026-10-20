//! M5Stack AtomS3 display implementation for Barracuda's stable display API.
//!
//! The AtomS3 family's 0.85-inch 128 x 128 module shipped with two
//! controllers: early units carry a GaladCore GC9107, newer units a Sitronix
//! ST7735S. Both share the wiring but differ in framebuffer window, mounting
//! rotation, and color inversion, so the controller is identified at start-up
//! from its `RDDID` answer, exactly as M5Stack's M5GFX `autodetect` does. The
//! read needs a three-wire SPI device: the panel answers on the same data
//! line the host writes on.
//!
//! The panel geometry below comes from M5GFX (`M5GFX.cpp`, `board_M5AtomS3`
//! and `board_M5AtomS3R`):
//!
//! | Controller | GRAM window | Offset | Mounting | Inverted |
//! | --- | --- | --- | --- | --- |
//! | GC9107 | 128 x 160 | x 0, y 32 | 0° | no |
//! | ST7735S | 132 x 132 | x 2, y 1 | 180° | yes |
//!
//! The backlight is a [`Backlight`]: a digital output on the AtomS3, an
//! LP5562 LED driver on the AtomS3R (see `atom-s3r-display`).

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
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::Rgb565,
    primitives::Rectangle,
};
use embedded_hal::{
    delay::DelayNs,
    digital::OutputPin,
    spi::{Operation, SpiDevice},
};
use mipidsi::{
    Builder, Display as MipidsiDisplay, InitError,
    dcs::SetAddressMode,
    interface::{Interface, SpiError, SpiInterface},
    models::{GC9107, Model, ModelInitError, ST7735s},
    options::{ColorInversion, ColorOrder, ModelOptions, Orientation, Rotation},
};

/// Visible panel width in pixels.
pub const WIDTH: u16 = 128;
/// Visible panel height in pixels.
pub const HEIGHT: u16 = 128;

/// MIPI DCS `RDDID` (read display identification).
const READ_DISPLAY_ID: u8 = 0x04;
/// Reset pulse and recovery used before the identification read.
const RESET_LOW_MS: u32 = 10;
const RESET_RECOVERY_MS: u32 = 120;
/// Full brightness on the portable `0..=255` scale.
const FULL_BRIGHTNESS: u8 = u8::MAX;

/// Takes the statically allocated transfer buffer used by the one panel.
///
/// Generated Board initialization constructs its display once, so the
/// returned buffer has one move-only owner for the lifetime of the application.
#[must_use]
pub fn take_transfer_buffer() -> &'static mut [u8; 512] {
    static BUFFER: static_cell::StaticCell<[u8; 512]> = static_cell::StaticCell::new();
    BUFFER.init([0; 512])
}

/// Panel controller identified at start-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelController {
    /// GaladCore GC9107 (early AtomS3 units).
    Gc9107,
    /// Sitronix ST7735S (newer AtomS3 units).
    St7735s,
}

impl PanelController {
    /// Classifies a display ID as M5GFX does: the ST7735S answers `7Ch 89h`
    /// or `83h 76h` in its first two bytes, the GC9107 `00h 91h 07h`.
    #[must_use]
    pub const fn from_display_id(id: u32) -> Option<Self> {
        let first_two = id & 0xffff;
        if first_two == 0x7683 || first_two == 0x897c {
            Some(Self::St7735s)
        } else if id & 0x00ff_ffff == 0x0007_9100 {
            Some(Self::Gc9107)
        } else {
            None
        }
    }

    const fn options(self) -> PanelOptions {
        match self {
            Self::Gc9107 => PanelOptions {
                offset: (0, 32),
                inversion: ColorInversion::Normal,
                mounting: Rotation::Deg0,
            },
            Self::St7735s => PanelOptions {
                offset: (2, 1),
                inversion: ColorInversion::Inverted,
                mounting: Rotation::Deg180,
            },
        }
    }
}

/// Decodes the five bytes clocked out after `RDDID`.
///
/// The controller inserts one dummy clock before its answer, so the four ID
/// bytes start at the second bit. The result holds the first byte received in
/// its least-significant byte, the order M5GFX compares against.
#[must_use]
pub fn decode_display_id(raw: [u8; 5]) -> u32 {
    let stream = raw
        .iter()
        .fold(0_u64, |stream, byte| (stream << 8) | u64::from(*byte));
    let aligned = (stream << 1) & 0x00ff_ffff_ffff;
    let bytes = ((aligned >> 8) as u32).to_be_bytes();
    u32::from_le_bytes(bytes)
}

#[derive(Clone, Copy)]
struct PanelOptions {
    offset: (u16, u16),
    inversion: ColorInversion,
    mounting: Rotation,
}

/// ST7735S model addressing the 132 x 132 GRAM window this module uses.
///
/// `mipidsi`'s [`ST7735s`] assumes the controller's full 132 x 162 memory,
/// which moves the rotated offsets; M5GFX sets `memory_height = 132`.
pub struct St7735s132;

impl Model for St7735s132 {
    type ColorFormat = Rgb565;
    const FRAMEBUFFER_SIZE: (u16, u16) = (132, 132);

    fn init<DELAY, DI>(
        &mut self,
        di: &mut DI,
        delay: &mut DELAY,
        options: &ModelOptions,
    ) -> Result<SetAddressMode, ModelInitError<DI::Error>>
    where
        DELAY: DelayNs,
        DI: Interface,
    {
        ST7735s.init(di, delay, options)
    }
}

/// Backlight control composed by an AtomS3-family display.
pub trait Backlight {
    /// Control failure.
    type Error: core::fmt::Debug;

    /// Sets the brightness on the portable `0..=255` scale; `0` turns it off.
    ///
    /// # Errors
    ///
    /// Returns the backlight's control failure.
    fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::Error>;
}

/// Backlight switched by an active-high digital output.
pub struct DigitalBacklight<P> {
    pin: P,
}

impl<P> DigitalBacklight<P> {
    /// Wraps the backlight enable output.
    #[must_use]
    pub const fn new(pin: P) -> Self {
        Self { pin }
    }
}

impl<P: OutputPin> Backlight for DigitalBacklight<P> {
    type Error = P::Error;

    fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::Error> {
        if brightness == 0 {
            self.pin.set_low()
        } else {
            self.pin.set_high()
        }
    }
}

/// Portable options of an AtomS3-family display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtomS3DisplayConfig {
    orientation: DisplayOrientation,
}

impl AtomS3DisplayConfig {
    /// Creates configuration with the initial orientation relative to the
    /// module's upright mounting.
    #[must_use]
    pub const fn new(orientation: DisplayOrientation) -> Self {
        Self { orientation }
    }
}

/// Move-only resources consumed by initialization.
pub struct AtomS3DisplayBindings<SPI, DC, RST, BL, DELAY> {
    spi: SPI,
    dc: DC,
    reset: RST,
    backlight: BL,
    delay: DELAY,
    buffer: &'static mut [u8; 512],
}

impl<SPI, DC, RST, BL, DELAY> AtomS3DisplayBindings<SPI, DC, RST, BL, DELAY> {
    /// Combines the three-wire SPI device, control outputs, backlight, delay
    /// source, and statically owned transfer buffer.
    #[must_use]
    pub const fn new(
        spi: SPI,
        dc: DC,
        reset: RST,
        backlight: BL,
        delay: DELAY,
        buffer: &'static mut [u8; 512],
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

    /// Brings up the backlight hardware with the delay source and replaces
    /// the backlight resource with the result, keeping every other resource.
    ///
    /// # Errors
    ///
    /// Returns the bring-up failure.
    pub fn try_map_backlight<B, E>(
        mut self,
        map: impl FnOnce(BL, &mut DELAY) -> Result<B, E>,
    ) -> Result<AtomS3DisplayBindings<SPI, DC, RST, B, DELAY>, E> {
        let backlight = map(self.backlight, &mut self.delay)?;
        Ok(AtomS3DisplayBindings {
            spi: self.spi,
            dc: self.dc,
            reset: self.reset,
            backlight,
            delay: self.delay,
            buffer: self.buffer,
        })
    }

    /// Replaces the backlight, keeping every other resource.
    pub fn map_backlight<B>(
        self,
        map: impl FnOnce(BL) -> B,
    ) -> AtomS3DisplayBindings<SPI, DC, RST, B, DELAY> {
        AtomS3DisplayBindings {
            spi: self.spi,
            dc: self.dc,
            reset: self.reset,
            backlight: map(self.backlight),
            delay: self.delay,
            buffer: self.buffer,
        }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum AtomS3InitError<SpiE, DcE, RstE, BlE> {
    /// The identification read failed on the SPI device.
    Spi(SpiE),
    /// The data/command output failed.
    Dc(DcE),
    /// The reset output failed.
    Reset(RstE),
    /// The backlight rejected its initial state.
    Backlight(BlE),
    /// Controller initialization failed.
    Display(InitError<SpiError<SpiE, DcE>, RstE>),
}

/// Runtime display operation failure.
#[derive(Debug)]
pub enum AtomS3DisplayError<SpiE, DcE, BlE> {
    /// The panel interface rejected a command.
    Interface(SpiError<SpiE, DcE>),
    /// The backlight rejected the requested state.
    Backlight(BlE),
}

type PanelInterface<SPI, DC> = SpiInterface<'static, SPI, DC>;

enum Panel<SPI, DC, RST>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
{
    Gc9107(MipidsiDisplay<PanelInterface<SPI, DC>, GC9107, RST>),
    St7735s(MipidsiDisplay<PanelInterface<SPI, DC>, St7735s132, RST>),
}

macro_rules! with_panel {
    ($panel:expr, $inner:ident => $body:expr) => {
        match $panel {
            Panel::Gc9107($inner) => $body,
            Panel::St7735s($inner) => $body,
        }
    };
}

/// Initialized AtomS3-family panel implementing Barracuda's display API.
pub struct AtomS3Display<SPI, DC, RST, BL, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
{
    panel: Panel<SPI, DC, RST>,
    controller: PanelController,
    backlight: BL,
    delay: DELAY,
    orientation: DisplayOrientation,
    brightness: u8,
}

/// [`AtomS3InitError`] for one set of bindings.
pub type InitErrorFor<SPI, DC, RST, BL> = AtomS3InitError<
    <SPI as embedded_hal::spi::ErrorType>::Error,
    <DC as embedded_hal::digital::ErrorType>::Error,
    <RST as embedded_hal::digital::ErrorType>::Error,
    <BL as Backlight>::Error,
>;

/// Result of [`initialize`].
pub type InitResult<SPI, DC, RST, BL, DELAY> =
    Result<AtomS3Display<SPI, DC, RST, BL, DELAY>, InitErrorFor<SPI, DC, RST, BL>>;

/// Resets the panel, identifies its controller, initializes it, and turns
/// the backlight on.
///
/// # Errors
///
/// Returns [`AtomS3InitError`] when a resource or the controller
/// initialization fails.
pub fn initialize<SPI, DC, RST, BL, DELAY>(
    bindings: AtomS3DisplayBindings<SPI, DC, RST, BL, DELAY>,
    config: AtomS3DisplayConfig,
) -> InitResult<SPI, DC, RST, BL, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
    BL: Backlight,
    DELAY: DelayNs,
{
    let AtomS3DisplayBindings {
        mut spi,
        mut dc,
        mut reset,
        mut backlight,
        mut delay,
        buffer,
    } = bindings;
    backlight
        .set_brightness(0)
        .map_err(AtomS3InitError::Backlight)?;
    reset.set_low().map_err(AtomS3InitError::Reset)?;
    delay.delay_ms(RESET_LOW_MS);
    reset.set_high().map_err(AtomS3InitError::Reset)?;
    delay.delay_ms(RESET_RECOVERY_MS);

    let id = read_display_id::<SPI, DC, RST, BL>(&mut spi, &mut dc)?;
    // M5GFX re-probes an unrecognized AtomS3R panel at 100 kHz because some
    // GC9107 batches answer only at low clocks, while a healthy ST7735S is
    // always recognized at the 8 MHz probe clock. One read clock is available
    // here, so an unrecognized answer at the Board's read clock selects the
    // GC9107, the controller of the units that answer unreliably.
    let controller = PanelController::from_display_id(id).unwrap_or(PanelController::Gc9107);
    let options = controller.options();
    let interface = SpiInterface::new(spi, dc, buffer);
    let orientation = mipidsi_orientation(options.mounting, config.orientation);
    let panel = match controller {
        PanelController::Gc9107 => Panel::Gc9107(
            Builder::new(GC9107, interface)
                .reset_pin(reset)
                .display_size(WIDTH, HEIGHT)
                .display_offset(options.offset.0, options.offset.1)
                .color_order(ColorOrder::Bgr)
                .invert_colors(options.inversion)
                .orientation(orientation)
                .init(&mut delay)
                .map_err(AtomS3InitError::Display)?,
        ),
        PanelController::St7735s => Panel::St7735s(
            Builder::new(St7735s132, interface)
                .reset_pin(reset)
                .display_size(WIDTH, HEIGHT)
                .display_offset(options.offset.0, options.offset.1)
                .color_order(ColorOrder::Bgr)
                .invert_colors(options.inversion)
                .orientation(orientation)
                .init(&mut delay)
                .map_err(AtomS3InitError::Display)?,
        ),
    };
    backlight
        .set_brightness(FULL_BRIGHTNESS)
        .map_err(AtomS3InitError::Backlight)?;
    Ok(AtomS3Display {
        panel,
        controller,
        backlight,
        delay,
        orientation: config.orientation,
        brightness: FULL_BRIGHTNESS,
    })
}

fn read_display_id<SPI, DC, RST, BL>(
    spi: &mut SPI,
    dc: &mut DC,
) -> Result<u32, InitErrorFor<SPI, DC, RST, BL>>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
    BL: Backlight,
{
    let mut raw = [0_u8; 5];
    dc.set_low().map_err(AtomS3InitError::Dc)?;
    spi.transaction(&mut [
        Operation::Write(&[READ_DISPLAY_ID]),
        Operation::Read(&mut raw),
    ])
    .map_err(AtomS3InitError::Spi)?;
    Ok(decode_display_id(raw))
}

impl<SPI, DC, RST, BL, DELAY> AtomS3Display<SPI, DC, RST, BL, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
{
    /// Returns the controller identified at start-up.
    #[must_use]
    pub const fn controller(&self) -> PanelController {
        self.controller
    }
}

impl<SPI, DC, RST, BL, DELAY> OriginDimensions for AtomS3Display<SPI, DC, RST, BL, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
{
    fn size(&self) -> Size {
        Size::new(u32::from(WIDTH), u32::from(HEIGHT))
    }
}

impl<SPI, DC, RST, BL, DELAY> DrawTarget for AtomS3Display<SPI, DC, RST, BL, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
{
    type Color = Rgb565;
    type Error = SpiError<SPI::Error, DC::Error>;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        with_panel!(&mut self.panel, panel => panel.draw_iter(pixels))
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        with_panel!(&mut self.panel, panel => panel.fill_contiguous(area, colors))
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        with_panel!(&mut self.panel, panel => panel.fill_solid(area, color))
    }
}

impl<SPI, DC, RST, BL, DELAY> Display for AtomS3Display<SPI, DC, RST, BL, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
    BL: Backlight,
    DELAY: DelayNs,
{
    type ControlError = AtomS3DisplayError<SPI::Error, DC::Error, BL::Error>;
    type RenderError = SpiError<SPI::Error, DC::Error>;

    fn descriptor(&self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Rgb565,
            DisplayTechnology::Lcd,
            DisplayFeatures::BRIGHTNESS | DisplayFeatures::ORIENTATION | DisplayFeatures::SLEEP,
        )
    }

    fn draw_rgb565(&mut self, area: Rectangle, pixels: &[Rgb565]) -> Result<(), Self::RenderError> {
        self.fill_contiguous(&area, pixels.iter().copied())
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        Ok(())
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        let delay = &mut self.delay;
        match power {
            DisplayPower::On => {
                with_panel!(&mut self.panel, panel => panel.wake(delay))
                    .map_err(AtomS3DisplayError::Interface)?;
                self.backlight
                    .set_brightness(self.brightness)
                    .map_err(AtomS3DisplayError::Backlight)
            }
            DisplayPower::Sleep | DisplayPower::Off => {
                self.backlight
                    .set_brightness(0)
                    .map_err(AtomS3DisplayError::Backlight)?;
                with_panel!(&mut self.panel, panel => panel.sleep(delay))
                    .map_err(AtomS3DisplayError::Interface)
            }
        }
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        self.backlight
            .set_brightness(brightness)
            .map_err(AtomS3DisplayError::Backlight)?;
        self.brightness = brightness;
        Ok(())
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        let mounting = self.controller.options().mounting;
        with_panel!(
            &mut self.panel,
            panel => panel.set_orientation(mipidsi_orientation(mounting, orientation))
        )
        .map_err(AtomS3DisplayError::Interface)?;
        self.orientation = orientation;
        Ok(())
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(())
    }
}

/// Static factory for the AtomS3, whose backlight is a digital output.
pub struct AtomS3DisplayImplementation<SPI, DC, RST, BL, DELAY>(
    FactoryTypes<SPI, DC, RST, BL, DELAY>,
);

/// Bindings carried only at the type level by a static factory.
pub type FactoryTypes<SPI, DC, RST, BL, DELAY> = PhantomData<fn() -> (SPI, DC, RST, BL, DELAY)>;

impl<SPI, DC, RST, BL, DELAY> PeripheralImplementation
    for AtomS3DisplayImplementation<SPI, DC, RST, BL, DELAY>
where
    SPI: SpiDevice + 'static,
    DC: OutputPin + 'static,
    RST: OutputPin + 'static,
    BL: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = AtomS3DisplayBindings<SPI, DC, RST, BL, DELAY>;
    type Config = AtomS3DisplayConfig;
    type Peripheral = AtomS3Display<SPI, DC, RST, DigitalBacklight<BL>, DELAY>;
    type Error = InitErrorFor<SPI, DC, RST, DigitalBacklight<BL>>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        initialize(bindings.map_backlight(DigitalBacklight::new), config)
    }
}

const fn mipidsi_orientation(mounting: Rotation, orientation: DisplayOrientation) -> Orientation {
    let rotation = match orientation {
        DisplayOrientation::Deg0 => Rotation::Deg0,
        DisplayOrientation::Deg90 => Rotation::Deg90,
        DisplayOrientation::Deg180 => Rotation::Deg180,
        DisplayOrientation::Deg270 => Rotation::Deg270,
    };
    Orientation::new().rotate(mounting).rotate(rotation)
}

#[cfg(test)]
mod tests {
    use super::{PanelController, decode_display_id};

    /// Encodes four ID bytes the way the panel clocks them out: one dummy
    /// bit, the bytes MSB first, then trailing bits.
    fn clocked(bytes: [u8; 4], trailing: u8) -> [u8; 5] {
        let stream = (u64::from(u32::from_be_bytes(bytes)) << 7) | u64::from(trailing >> 1);
        let raw = stream.to_be_bytes();
        [raw[3], raw[4], raw[5], raw[6], raw[7]]
    }

    #[test]
    fn decodes_the_id_after_the_dummy_bit() {
        assert_eq!(
            decode_display_id(clocked([0x7c, 0x89, 0xf0, 0x00], 0xfe)),
            0x00f0_897c
        );
        assert_eq!(
            decode_display_id(clocked([0x00, 0x91, 0x07, 0x5a], 0)),
            0x5a07_9100
        );
    }

    #[test]
    fn classifies_both_atom_s3_controllers() {
        assert_eq!(
            PanelController::from_display_id(0x00f0_897c),
            Some(PanelController::St7735s)
        );
        assert_eq!(
            PanelController::from_display_id(0x0000_7683),
            Some(PanelController::St7735s)
        );
        assert_eq!(
            PanelController::from_display_id(0xff07_9100),
            Some(PanelController::Gc9107)
        );
        assert_eq!(PanelController::from_display_id(0), None);
        assert_eq!(PanelController::from_display_id(0xffff_ffff), None);
    }
}

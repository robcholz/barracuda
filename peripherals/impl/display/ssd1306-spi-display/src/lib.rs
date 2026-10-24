//! SSD1306 monochrome OLED display implementation for Barracuda's display API.
//!
//! Pixels are drawn into a framebuffer in the controller's page layout and
//! sent by [`Display::flush`]. Portable RGB565 pixels light when their
//! luminance reaches half scale. Orientation is applied while drawing, so a
//! quarter turn swaps the logical width and height of non-square glasses.

#![no_std]

use core::{convert::Infallible, marker::PhantomData};

use barracuda_driver_ssd1306::{ErrorFor as ControllerErrorFor, Geometry, Ssd1306};
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
    pixelcolor::{BinaryColor, Rgb565, RgbColor},
    primitives::Rectangle,
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, spi::SpiDevice};

/// Bytes of the largest SSD1306 frame (128 x 64 pixels).
const MAX_FRAME_LEN: usize = 1024;
/// Reset pulse and recovery.
const RESET_LOW_MS: u32 = 10;
const RESET_RECOVERY_MS: u32 = 10;
/// Contrast after initialization, the middle of the portable brightness scale.
const DEFAULT_CONTRAST: u8 = 0x7f;

/// Glass geometry and initial orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ssd1306DisplayConfig {
    geometry: Geometry,
    orientation: DisplayOrientation,
}

impl Ssd1306DisplayConfig {
    /// Describes a `width` x `height` glass at GDDRAM column `column_offset`
    /// with its `DAh` COM pins byte, and the initial orientation.
    #[must_use]
    pub const fn new(
        width: u8,
        height: u8,
        column_offset: u8,
        com_pins: u8,
        orientation: DisplayOrientation,
    ) -> Self {
        Self {
            geometry: Geometry::new(width, height, column_offset, com_pins),
            orientation,
        }
    }

    /// Returns the stable peripheral descriptor.
    #[must_use]
    pub fn descriptor(self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            logical_size(self.geometry, self.orientation),
            PixelFormat::Binary,
            DisplayTechnology::Oled,
            DisplayFeatures::BUFFERED
                | DisplayFeatures::BRIGHTNESS
                | DisplayFeatures::ORIENTATION
                | DisplayFeatures::SLEEP,
        )
    }
}

/// Move-only resources consumed by initialization.
pub struct Ssd1306DisplayBindings<SPI, DC, RST, DELAY> {
    spi: SPI,
    dc: DC,
    reset: RST,
    delay: DELAY,
}

impl<SPI, DC, RST, DELAY> Ssd1306DisplayBindings<SPI, DC, RST, DELAY> {
    /// Combines the SPI device, data/command and reset outputs, and delay.
    #[must_use]
    pub const fn new(spi: SPI, dc: DC, reset: RST, delay: DELAY) -> Self {
        Self {
            spi,
            dc,
            reset,
            delay,
        }
    }
}

/// Initialization or control failure.
#[derive(Debug)]
pub enum Ssd1306DisplayError<ControllerError, ResetError> {
    /// The controller rejected its geometry or a transfer failed.
    Controller(ControllerError),
    /// The reset output failed.
    Reset(ResetError),
}

/// [`Ssd1306DisplayError`] for one set of bindings.
pub type ErrorFor<SPI, DC, RST> = Ssd1306DisplayError<
    ControllerErrorFor<SPI, DC>,
    <RST as embedded_hal::digital::ErrorType>::Error,
>;

/// Initialized SSD1306 panel.
pub struct Ssd1306Display<SPI, DC, RST, DELAY> {
    controller: Ssd1306<SPI, DC>,
    _reset: RST,
    _delay: DELAY,
    frame: [u8; MAX_FRAME_LEN],
    orientation: DisplayOrientation,
}

/// Bindings carried only at the type level by the static factory.
type FactoryTypes<SPI, DC, RST, DELAY> = PhantomData<fn() -> (SPI, DC, RST, DELAY)>;

/// Static factory used by generated Board composition.
pub struct Ssd1306DisplayImplementation<SPI, DC, RST, DELAY>(FactoryTypes<SPI, DC, RST, DELAY>);

impl<SPI, DC, RST, DELAY> PeripheralImplementation
    for Ssd1306DisplayImplementation<SPI, DC, RST, DELAY>
where
    SPI: SpiDevice + 'static,
    DC: OutputPin + 'static,
    RST: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Ssd1306DisplayBindings<SPI, DC, RST, DELAY>;
    type Config = Ssd1306DisplayConfig;
    type Peripheral = Ssd1306Display<SPI, DC, RST, DELAY>;
    type Error = ErrorFor<SPI, DC, RST>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let Ssd1306DisplayBindings {
            spi,
            dc,
            mut reset,
            mut delay,
        } = bindings;
        let mut controller =
            Ssd1306::new(spi, dc, config.geometry).map_err(Ssd1306DisplayError::Controller)?;
        reset.set_low().map_err(Ssd1306DisplayError::Reset)?;
        delay.delay_ms(RESET_LOW_MS);
        reset.set_high().map_err(Ssd1306DisplayError::Reset)?;
        delay.delay_ms(RESET_RECOVERY_MS);
        controller
            .init(DEFAULT_CONTRAST)
            .map_err(Ssd1306DisplayError::Controller)?;
        let mut display = Ssd1306Display {
            controller,
            _reset: reset,
            _delay: delay,
            frame: [0; MAX_FRAME_LEN],
            orientation: config.orientation,
        };
        display.send_frame()?;
        Ok(display)
    }
}

impl<SPI, DC, RST, DELAY> Ssd1306Display<SPI, DC, RST, DELAY> {
    fn geometry(&self) -> Geometry
    where
        SPI: SpiDevice,
        DC: OutputPin,
    {
        self.controller.geometry()
    }
}

impl<SPI, DC, RST, DELAY> Ssd1306Display<SPI, DC, RST, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
{
    fn send_frame(&mut self) -> Result<(), ErrorFor<SPI, DC, RST>> {
        let length = self.geometry().frame_len();
        let offset = self.controller.column_offset(false);
        let frame = self.frame.get(..length).unwrap_or(&[]);
        self.controller
            .write_pages(0, offset, frame)
            .map_err(Ssd1306DisplayError::Controller)
    }

    fn set_pixel(&mut self, x: i32, y: i32, on: bool) {
        let geometry = self.geometry();
        if let Some(index) = frame_bit(geometry, self.orientation, x, y) {
            let (byte, mask) = index;
            if let Some(cell) = self.frame.get_mut(byte) {
                if on {
                    *cell |= mask;
                } else {
                    *cell &= !mask;
                }
            }
        }
    }
}

/// Returns the logical drawing size for an orientation.
fn logical_size(geometry: Geometry, orientation: DisplayOrientation) -> Size {
    let (width, height) = (u32::from(geometry.width()), u32::from(geometry.height()));
    match orientation {
        DisplayOrientation::Deg0 | DisplayOrientation::Deg180 => Size::new(width, height),
        DisplayOrientation::Deg90 | DisplayOrientation::Deg270 => Size::new(height, width),
    }
}

/// Maps a logical pixel to its framebuffer byte and bit mask.
fn frame_bit(
    geometry: Geometry,
    orientation: DisplayOrientation,
    x: i32,
    y: i32,
) -> Option<(usize, u8)> {
    let logical = logical_size(geometry, orientation);
    let x = u32::try_from(x).ok().filter(|x| *x < logical.width)?;
    let y = u32::try_from(y).ok().filter(|y| *y < logical.height)?;
    let width = u32::from(geometry.width());
    let height = u32::from(geometry.height());
    let last_column = width.checked_sub(1)?;
    let last_row = height.checked_sub(1)?;
    let (column, row) = match orientation {
        DisplayOrientation::Deg0 => (x, y),
        DisplayOrientation::Deg90 => (last_column.checked_sub(y)?, x),
        DisplayOrientation::Deg180 => (last_column.checked_sub(x)?, last_row.checked_sub(y)?),
        DisplayOrientation::Deg270 => (y, last_row.checked_sub(x)?),
    };
    let byte = (row >> 3).checked_mul(width)?.checked_add(column)?;
    Some((usize::try_from(byte).ok()?, 1 << (row & 7)))
}

/// Whether an RGB565 pixel lights: Rec. 601 luminance at half scale or more.
fn lights(color: Rgb565) -> bool {
    let scale = |value: u8, max: u8| u32::from(value) * 255 / u32::from(max);
    let red = scale(color.r(), Rgb565::MAX_R);
    let green = scale(color.g(), Rgb565::MAX_G);
    let blue = scale(color.b(), Rgb565::MAX_B);
    red * 299 + green * 587 + blue * 114 >= 128 * 1000
}

impl<SPI, DC, RST, DELAY> OriginDimensions for Ssd1306Display<SPI, DC, RST, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
{
    fn size(&self) -> Size {
        logical_size(self.geometry(), self.orientation)
    }
}

impl<SPI, DC, RST, DELAY> DrawTarget for Ssd1306Display<SPI, DC, RST, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
{
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            self.set_pixel(point.x, point.y, color.is_on());
        }
        Ok(())
    }
}

impl<SPI, DC, RST, DELAY> Display for Ssd1306Display<SPI, DC, RST, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
{
    type ControlError = ErrorFor<SPI, DC, RST>;
    type RenderError = Infallible;

    fn descriptor(&self) -> DisplayDescriptor {
        Ssd1306DisplayConfig {
            geometry: self.geometry(),
            orientation: self.orientation,
        }
        .descriptor()
    }

    fn draw_rgb565(&mut self, area: Rectangle, pixels: &[Rgb565]) -> Result<(), Self::RenderError> {
        self.fill_contiguous(
            &area,
            pixels.iter().map(|color| BinaryColor::from(lights(*color))),
        )
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        self.send_frame()
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        self.controller
            .set_display_on(power == DisplayPower::On)
            .map_err(Ssd1306DisplayError::Controller)
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        self.controller
            .set_contrast(brightness)
            .map_err(Ssd1306DisplayError::Controller)
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        self.orientation = orientation;
        Ok(())
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use barracuda_driver_ssd1306::Geometry;
    use barracuda_peripheral::display::{DisplayOrientation, PixelFormat};
    use embedded_graphics_core::{geometry::Size, pixelcolor::Rgb565, prelude::RgbColor};

    use super::{Ssd1306DisplayConfig, frame_bit, lights};

    const GLASS: Geometry = Geometry::new(64, 48, 32, 0x12);

    #[test]
    fn quarter_turns_swap_the_logical_size() {
        let upright = Ssd1306DisplayConfig::new(64, 48, 32, 0x12, DisplayOrientation::Deg0);
        let turned = Ssd1306DisplayConfig::new(64, 48, 32, 0x12, DisplayOrientation::Deg90);
        assert_eq!(upright.descriptor().size(), Size::new(64, 48));
        assert_eq!(turned.descriptor().size(), Size::new(48, 64));
        assert_eq!(upright.descriptor().pixel_format(), PixelFormat::Binary);
    }

    #[test]
    fn pixels_map_to_page_bytes_in_every_orientation() {
        // Top-left logical pixel.
        assert_eq!(
            frame_bit(GLASS, DisplayOrientation::Deg0, 0, 0),
            Some((0, 1))
        );
        assert_eq!(
            frame_bit(GLASS, DisplayOrientation::Deg90, 0, 0),
            Some((63, 1))
        );
        assert_eq!(
            frame_bit(GLASS, DisplayOrientation::Deg180, 0, 0),
            Some((5 * 64 + 63, 0x80))
        );
        assert_eq!(
            frame_bit(GLASS, DisplayOrientation::Deg270, 0, 0),
            Some((5 * 64, 0x80))
        );
        // Row 9 sits in page 1, bit 1.
        assert_eq!(
            frame_bit(GLASS, DisplayOrientation::Deg0, 3, 9),
            Some((64 + 3, 2))
        );
        // Outside the glass.
        assert_eq!(frame_bit(GLASS, DisplayOrientation::Deg0, 64, 0), None);
        assert_eq!(frame_bit(GLASS, DisplayOrientation::Deg90, 48, 0), None);
        assert_eq!(frame_bit(GLASS, DisplayOrientation::Deg0, -1, 0), None);
    }

    #[test]
    fn half_luminance_lights_a_pixel() {
        assert!(lights(Rgb565::WHITE));
        assert!(lights(Rgb565::GREEN));
        assert!(!lights(Rgb565::BLACK));
        assert!(!lights(Rgb565::BLUE));
    }
}

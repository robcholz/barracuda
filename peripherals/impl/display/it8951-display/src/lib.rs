//! Buffered 16-level e-paper implementation of Barracuda's stable display
//! peripheral over an ITE IT8951 timing controller.
//!
//! The IT8951 owns the panel's waveforms and its own image buffer; the host
//! keeps a 4-bit-per-pixel framebuffer, loads the changed rectangle into the
//! controller's buffer, and asks for a display update with one of the
//! controller's built-in waveform modes. The bring-up follows M5Stack's M5GFX
//! `Panel_IT8951` for the M5Paper: cycle the panel supply, wait for `HRDY`,
//! read the device information for the image buffer address (falling back to
//! M5Paper's `0x001236E0`), `SYS_RUN`, enable packed writes, and point image
//! loads at the buffer.
//!
//! RGB565 input is reduced to 16 gray levels with M5GFX's 4 x 4 ordered
//! dither (`(R + 2G + B + bayer) >> 6`).

#![no_std]

extern crate alloc;

use alloc::{vec, vec::Vec};
use core::{convert::Infallible, marker::PhantomData};

use barracuda_driver_it8951::{
    self as it8951, Area, It8951, M5PAPER_DEFAULT_IMAGE_BUFFER, PixelDepth, Rotation, UpdateMode,
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
    pixelcolor::{Gray4, GrayColor, Rgb565, RgbColor},
    primitives::Rectangle,
};
use embedded_hal::{
    delay::DelayNs,
    digital::{InputPin, OutputPin},
    spi::SpiDevice,
};

/// Optional operations of every IT8951 display.
pub const FEATURES: DisplayFeatures = DisplayFeatures::BUFFERED
    .union(DisplayFeatures::PARTIAL_REFRESH)
    .union(DisplayFeatures::BUSY)
    .union(DisplayFeatures::SLEEP);

/// Panel supply low time before power-up (M5GFX `Panel_Device::init`: 8 ms).
const POWER_OFF_MS: u32 = 8;
/// Settle time after the supply rises (M5GFX: 64 ms).
const POWER_ON_SETTLE_MS: u32 = 64;

/// M5GFX's signed 4 x 4 Bayer offsets (`bayer_4x4_signed`).
const BAYER_4X4_SIGNED: [i16; 16] = [
    -30, 2, -22, 10, 18, -14, 26, -6, -18, 14, -26, 6, 30, -2, 22, -10,
];

/// Native geometry and mounting of one IT8951 panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct It8951DisplayConfig {
    width: u16,
    height: u16,
    mounting: DisplayOrientation,
}

impl It8951DisplayConfig {
    /// Describes a `width` x `height` panel (scan direction) turned clockwise
    /// by `mounting` to the Board's upright view.
    ///
    /// Out-of-range dimensions are clamped to `1..=4096`.
    #[must_use]
    pub fn new(width: i64, height: i64, mounting: DisplayOrientation) -> Self {
        let clamp = |value: i64| u16::try_from(value.clamp(1, 4096)).unwrap_or(1);
        Self {
            width: clamp(width),
            height: clamp(height),
            mounting,
        }
    }

    /// Returns the upright drawing size.
    #[must_use]
    pub fn size(self) -> Size {
        let (width, height) = (u32::from(self.width), u32::from(self.height));
        if matches!(
            self.mounting,
            DisplayOrientation::Deg90 | DisplayOrientation::Deg270
        ) {
            Size::new(height, width)
        } else {
            Size::new(width, height)
        }
    }

    /// Returns the descriptor of a display with this geometry.
    #[must_use]
    pub fn descriptor(self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Gray4,
            DisplayTechnology::Epaper,
            FEATURES,
        )
    }

    /// Maps an upright drawing coordinate to the panel's scan coordinate, as
    /// M5GFX `_rotate_pos` does for its `offset_rotation`.
    #[must_use]
    pub fn to_native(self, x: u16, y: u16) -> (u16, u16) {
        let (width, height) = (self.width, self.height);
        let last = |extent: u16, value: u16| extent.saturating_sub(1).saturating_sub(value);
        match self.mounting {
            DisplayOrientation::Deg0 => (x, y),
            DisplayOrientation::Deg90 => (last(width, y), x),
            DisplayOrientation::Deg180 => (last(width, x), last(height, y)),
            DisplayOrientation::Deg270 => (y, last(height, x)),
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct It8951Bindings<SPI, READY, PANEL, HOLD, DELAY> {
    spi: SPI,
    ready: READY,
    panel_power: PANEL,
    power_hold: HOLD,
    delay: DELAY,
}

impl<SPI, READY, PANEL, HOLD, DELAY> It8951Bindings<SPI, READY, PANEL, HOLD, DELAY> {
    /// Combines the controller's SPI device, `HRDY` input, panel supply, and
    /// Board power latch.
    #[must_use]
    pub const fn new(
        spi: SPI,
        ready: READY,
        panel_power: PANEL,
        power_hold: HOLD,
        delay: DELAY,
    ) -> Self {
        Self {
            spi,
            ready,
            panel_power,
            power_hold,
            delay,
        }
    }
}

/// Initialization or display-control failure.
#[derive(Debug)]
pub enum It8951DisplayError<SPI, READY, PANEL, HOLD> {
    /// The controller transaction failed or timed out.
    Controller(it8951::Error<SPI, READY>),
    /// The panel supply output failed.
    PanelPower(PANEL),
    /// The Board power latch failed.
    PowerHold(HOLD),
    /// This panel has no brightness control.
    BrightnessUnsupported,
    /// Only the Board's upright orientation is supported.
    OrientationUnsupported,
}

impl<SPI, READY, PANEL, HOLD> From<it8951::Error<SPI, READY>>
    for It8951DisplayError<SPI, READY, PANEL, HOLD>
{
    fn from(error: it8951::Error<SPI, READY>) -> Self {
        Self::Controller(error)
    }
}

/// Error type of one concrete display.
pub type ErrorOf<SPI, READY, PANEL, HOLD> = It8951DisplayError<
    <SPI as embedded_hal::spi::ErrorType>::Error,
    <READY as embedded_hal::digital::ErrorType>::Error,
    <PANEL as embedded_hal::digital::ErrorType>::Error,
    <HOLD as embedded_hal::digital::ErrorType>::Error,
>;

/// Initialized IT8951 display.
pub struct It8951Display<SPI, READY, PANEL, HOLD, DELAY> {
    controller: It8951<SPI, READY>,
    panel_power: PANEL,
    power_hold: HOLD,
    delay: DELAY,
    config: It8951DisplayConfig,
    image_buffer: u32,
    /// Native 4-bit framebuffer, two pixels per byte, first pixel high.
    framebuffer: Vec<u8>,
    asleep: bool,
}

/// Static implementation factory.
pub struct It8951DisplayImplementation<SPI, READY, PANEL, HOLD, DELAY>(
    PhantomData<(SPI, READY, PANEL, HOLD, DELAY)>,
);

impl<SPI, READY, PANEL, HOLD, DELAY> PeripheralImplementation
    for It8951DisplayImplementation<SPI, READY, PANEL, HOLD, DELAY>
where
    SPI: SpiDevice + 'static,
    READY: InputPin + 'static,
    PANEL: OutputPin + 'static,
    HOLD: OutputPin + 'static,
    DELAY: DelayNs + 'static,
    SPI::Error: 'static,
    READY::Error: 'static,
    PANEL::Error: 'static,
    HOLD::Error: 'static,
{
    type Bindings = It8951Bindings<SPI, READY, PANEL, HOLD, DELAY>;
    type Config = It8951DisplayConfig;
    type Peripheral = It8951Display<SPI, READY, PANEL, HOLD, DELAY>;
    type Error = ErrorOf<SPI, READY, PANEL, HOLD>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let It8951Bindings {
            spi,
            ready,
            panel_power,
            mut power_hold,
            delay,
        } = bindings;
        power_hold
            .set_high()
            .map_err(It8951DisplayError::PowerHold)?;
        let bytes = usize::from(config.width)
            .div_ceil(2)
            .saturating_mul(usize::from(config.height));
        let mut display = It8951Display {
            controller: It8951::new(spi, ready),
            panel_power,
            power_hold,
            delay,
            config,
            image_buffer: M5PAPER_DEFAULT_IMAGE_BUFFER,
            framebuffer: vec![0xFF; bytes],
            asleep: false,
        };
        display.bring_up()?;
        Ok(display)
    }
}

impl<SPI, READY, PANEL, HOLD, DELAY> It8951Display<SPI, READY, PANEL, HOLD, DELAY>
where
    SPI: SpiDevice,
    READY: InputPin,
    PANEL: OutputPin,
    HOLD: OutputPin,
    DELAY: DelayNs,
{
    /// Returns the controller's image buffer address in use.
    #[must_use]
    pub const fn image_buffer_address(&self) -> u32 {
        self.image_buffer
    }

    /// Cycles the panel supply and runs M5GFX's `Panel_IT8951::init`.
    fn bring_up(&mut self) -> Result<(), ErrorOf<SPI, READY, PANEL, HOLD>> {
        self.panel_power
            .set_low()
            .map_err(It8951DisplayError::PanelPower)?;
        self.delay.delay_ms(POWER_OFF_MS);
        self.panel_power
            .set_high()
            .map_err(It8951DisplayError::PanelPower)?;
        self.delay.delay_ms(POWER_ON_SETTLE_MS);
        let delay = &mut self.delay;
        self.controller
            .wait_ready(delay, it8951::DEFAULT_READY_TIMEOUT_MS)?;
        let info = self.controller.device_info(delay)?;
        self.image_buffer = if info.is_valid() {
            info.image_buffer_address
        } else {
            M5PAPER_DEFAULT_IMAGE_BUFFER
        };
        self.controller.system_run(delay)?;
        self.controller.enable_packed_write(delay)?;
        self.controller
            .set_image_buffer_address(delay, self.image_buffer)?;
        self.asleep = false;
        Ok(())
    }

    fn row_bytes(&self) -> usize {
        usize::from(self.config.width).div_ceil(2)
    }

    fn set_native(&mut self, x: u16, y: u16, level: u8) {
        if x >= self.config.width || y >= self.config.height {
            return;
        }
        let index = usize::from(y)
            .saturating_mul(self.row_bytes())
            .saturating_add(usize::from(x / 2));
        if let Some(byte) = self.framebuffer.get_mut(index) {
            *byte = if x.is_multiple_of(2) {
                (*byte & 0x0F) | (level << 4)
            } else {
                (*byte & 0xF0) | (level & 0x0F)
            };
        }
    }

    fn set_pixel(&mut self, point: Point, level: u8) {
        let size = self.config.size();
        let (Ok(x), Ok(y)) = (u16::try_from(point.x), u16::try_from(point.y)) else {
            return;
        };
        if u32::from(x) >= size.width || u32::from(y) >= size.height {
            return;
        }
        let (native_x, native_y) = self.config.to_native(x, y);
        self.set_native(native_x, native_y, level);
    }

    /// Native rectangle covering `region`, widened to whole four-pixel words.
    fn native_area(&self, region: Option<Rectangle>) -> Option<Area> {
        let size = self.config.size();
        let upright = Rectangle::new(Point::zero(), size);
        let region = region.map_or(upright, |region| region.intersection(&upright));
        let bottom_right = region.bottom_right()?;
        let corner = |point: Point| {
            self.config.to_native(
                u16::try_from(point.x).unwrap_or(0),
                u16::try_from(point.y).unwrap_or(0),
            )
        };
        let (ax, ay) = corner(region.top_left);
        let (bx, by) = corner(bottom_right);
        let left = ax.min(bx) & !3;
        let right = (ax.max(bx) | 3).min(self.config.width.saturating_sub(1));
        let top = ay.min(by);
        let bottom = ay.max(by);
        Some(Area {
            x: left,
            y: top,
            width: right.saturating_sub(left).saturating_add(1),
            height: bottom.saturating_sub(top).saturating_add(1),
        })
    }

    fn load_area(&mut self, area: Area) -> Result<(), ErrorOf<SPI, READY, PANEL, HOLD>> {
        let row_bytes = self.row_bytes();
        let first = usize::from(area.x / 2);
        let length = usize::from(area.width).div_ceil(2);
        let delay = &mut self.delay;
        // Never load over a region whose update is still running.
        self.controller
            .wait_display_done(delay, it8951::DEFAULT_DISPLAY_TIMEOUT_MS)?;
        self.controller
            .load_image_area(delay, PixelDepth::Bpp4, Rotation::Deg0, area)?;
        for row in area.y..area.y.saturating_add(area.height) {
            let start = usize::from(row)
                .saturating_mul(row_bytes)
                .saturating_add(first);
            let Some(bytes) = self.framebuffer.get(start..start.saturating_add(length)) else {
                break;
            };
            self.controller.write_pixels(delay, bytes)?;
        }
        self.controller.load_image_end(delay)?;
        Ok(())
    }
}

/// M5GFX's 16-level reduction of one RGB565 pixel at upright `(x, y)`.
#[must_use]
pub fn dithered_level(pixel: Rgb565, x: i32, y: i32) -> u8 {
    let expand = |value: u8, bits: u32| {
        let max = (1_u16 << bits) - 1;
        i16::try_from(u16::from(value) * 255 / max).unwrap_or(255)
    };
    let sum = expand(pixel.r(), 5) + 2 * expand(pixel.g(), 6) + expand(pixel.b(), 5);
    let index = usize::try_from((y & 3) << 2 | (x & 3)).unwrap_or(0);
    let biased = (sum + BAYER_4X4_SIGNED[index]).max(0) >> 6;
    u8::try_from(biased.min(15)).unwrap_or(15)
}

impl<SPI, READY, PANEL, HOLD, DELAY> OriginDimensions
    for It8951Display<SPI, READY, PANEL, HOLD, DELAY>
{
    fn size(&self) -> Size {
        self.config.size()
    }
}

impl<SPI, READY, PANEL, HOLD, DELAY> DrawTarget for It8951Display<SPI, READY, PANEL, HOLD, DELAY>
where
    SPI: SpiDevice,
    READY: InputPin,
    PANEL: OutputPin,
    HOLD: OutputPin,
    DELAY: DelayNs,
{
    type Color = Gray4;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            self.set_pixel(point, color.luma());
        }
        Ok(())
    }
}

impl<SPI, READY, PANEL, HOLD, DELAY> Display for It8951Display<SPI, READY, PANEL, HOLD, DELAY>
where
    SPI: SpiDevice,
    READY: InputPin,
    PANEL: OutputPin,
    HOLD: OutputPin,
    DELAY: DelayNs,
{
    type ControlError = ErrorOf<SPI, READY, PANEL, HOLD>;
    type RenderError = Infallible;

    fn descriptor(&self) -> DisplayDescriptor {
        self.config.descriptor()
    }

    fn draw_rgb565(&mut self, area: Rectangle, pixels: &[Rgb565]) -> Result<(), Self::RenderError> {
        let width = i32::try_from(area.size.width).unwrap_or(0);
        if width == 0 {
            return Ok(());
        }
        let mut offset = 0_i32;
        for pixel in pixels {
            let point = area.top_left + Point::new(offset % width, offset / width);
            self.set_pixel(point, dithered_level(*pixel, point.x, point.y));
            offset = offset.saturating_add(1);
        }
        Ok(())
    }

    async fn flush(&mut self, request: RefreshRequest) -> Result<(), Self::ControlError> {
        let Some(area) = self.native_area(request.region()) else {
            return Ok(());
        };
        // M5GFX: quality GC16, text GL16, fast DU, fastest DU4.
        let mode = match request.mode() {
            RefreshMode::Automatic | RefreshMode::Full => UpdateMode::Gc16,
            RefreshMode::Partial => UpdateMode::Gl16,
            RefreshMode::Fast => UpdateMode::Du4,
        };
        self.load_area(area)?;
        self.controller
            .display_area(&mut self.delay, area, mode, self.image_buffer)?;
        Ok(())
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        match power {
            DisplayPower::On => {
                self.power_hold
                    .set_high()
                    .map_err(It8951DisplayError::PowerHold)?;
                if self.asleep {
                    // M5GFX wakes the IT8951 by running its initialization again.
                    self.bring_up()?;
                } else {
                    self.controller.system_run(&mut self.delay)?;
                }
            }
            DisplayPower::Sleep => {
                self.controller
                    .wait_display_done(&mut self.delay, it8951::DEFAULT_DISPLAY_TIMEOUT_MS)?;
                self.controller.sleep(&mut self.delay)?;
                self.asleep = true;
            }
            DisplayPower::Off => {
                self.controller
                    .wait_display_done(&mut self.delay, it8951::DEFAULT_DISPLAY_TIMEOUT_MS)?;
                self.controller.sleep(&mut self.delay)?;
                self.panel_power
                    .set_low()
                    .map_err(It8951DisplayError::PanelPower)?;
                self.asleep = true;
            }
        }
        Ok(())
    }

    async fn set_brightness(&mut self, _brightness: u8) -> Result<(), Self::ControlError> {
        Err(It8951DisplayError::BrightnessUnsupported)
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        if orientation == DisplayOrientation::Deg0 {
            Ok(())
        } else {
            Err(It8951DisplayError::OrientationUnsupported)
        }
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(self
            .controller
            .wait_display_done(&mut self.delay, it8951::DEFAULT_DISPLAY_TIMEOUT_MS)?)
    }
}

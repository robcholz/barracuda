//! M5Stack StopWatch display implementation for Barracuda's display API.
//!
//! The StopWatch carries a 1.75-inch round AMOLED driven by a Chipone CO5300
//! over QSPI. Its reset and supply enables sit on the M5IOE1 expander of the
//! internal I2C bus, which stays private to this implementation. Geometry and
//! bring-up follow M5Stack's M5GFX (`board_M5StopWatch`, `Panel_StopWatch`):
//! a 468 x 468 window six columns into the controller's 480 x 480 GRAM,
//! covering the 466-pixel glass.
//!
//! The CO5300 only accepts windows on its two-pixel grid. A shadow
//! framebuffer in bulk memory therefore holds every pixel, so any drawn
//! rectangle can be widened to that grid and sent with its true neighbors.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::QuadSpiBus;
use barracuda_bulk_memory::{BulkAllocError, BulkBox};
use barracuda_driver_co5300::{
    COLOR_WRITE_OPCODE, COLUMN_ADDRESS, COMMAND_WRITE_OPCODE, DISPLAY_OFF, DISPLAY_ON,
    M5STACK_STOPWATCH_INIT, MEMORY_WRITE, ROW_ADDRESS, SLEEP_IN, SLEEP_OUT, SLEEP_SETTLE_MS,
    WRITE_BRIGHTNESS, Window, command_address,
};
use barracuda_driver_m5ioe1::{Error as ExpanderError, M5ioe1};
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
    pixelcolor::{IntoStorage, Rgb565},
    primitives::Rectangle,
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

/// Side of the square drawing window in pixels (M5GFX `panel_width`).
pub const SIDE: u16 = 468;
/// GRAM column of the window's first pixel (M5GFX `offset_x`).
const COLUMN_OFFSET: u16 = 6;
/// GRAM row of the window's first pixel (M5GFX `offset_y`).
const ROW_OFFSET: u16 = 0;
/// Pixels of one window row.
const ROW_PIXELS: usize = SIDE as usize;
/// Pixels of the whole window.
const FRAME_PIXELS: usize = ROW_PIXELS * ROW_PIXELS;

/// M5IOE1 `IO4`, pulsed with the panel reset (M5GFX: touch reset; M5Unified:
/// display power).
const EXPANDER_IO4: u8 = 4;
/// M5IOE1 `IO5`, the OLED reset (active low).
const OLED_RESET: u8 = 5;
/// M5IOE1 `IO8`, `L3B_EN`, set high by M5GFX before the panel starts.
const RAIL_ENABLE: u8 = 8;
/// M5GFX's reset timing: settle, pulse low, recover.
const RESET_SETTLE_MS: u32 = 10;
const RESET_LOW_MS: u32 = 8;
const RESET_RECOVERY_MS: u32 = 2;

/// Board options of the StopWatch display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StopWatchDisplayConfig {
    expander_address: u8,
    orientation: DisplayOrientation,
}

impl StopWatchDisplayConfig {
    /// Creates configuration from the M5IOE1's I2C address and the initial
    /// orientation.
    #[must_use]
    pub const fn new(expander_address: u8, orientation: DisplayOrientation) -> Self {
        Self {
            expander_address,
            orientation,
        }
    }
}

/// Move-only resources consumed by initialization.
pub struct StopWatchDisplayBindings<BUS, I2C, DELAY> {
    bus: BUS,
    i2c: I2C,
    delay: DELAY,
}

impl<BUS, I2C, DELAY> StopWatchDisplayBindings<BUS, I2C, DELAY> {
    /// Combines the QSPI device, the expander's I2C bus, and a delay.
    #[must_use]
    pub const fn new(bus: BUS, i2c: I2C, delay: DELAY) -> Self {
        Self { bus, i2c, delay }
    }
}

/// Initialization, rendering, or control failure.
#[derive(Debug)]
pub enum StopWatchDisplayError<BusError, I2cError> {
    /// The QSPI transport failed.
    Bus(BusError),
    /// The M5IOE1 could not be woken or its outputs configured.
    Expander(ExpanderError<I2cError>),
    /// Bulk memory could not hold the shadow framebuffer.
    Memory(BulkAllocError),
    /// A pixel region was outside the panel or had the wrong length.
    InvalidPixelRegion,
}

/// [`StopWatchDisplayError`] for one set of bindings.
pub type ErrorFor<BUS, I2C> =
    StopWatchDisplayError<<BUS as QuadSpiBus>::Error, <I2C as embedded_hal::i2c::ErrorType>::Error>;

/// Initialized StopWatch display.
pub struct StopWatchDisplay<BUS, I2C, DELAY> {
    bus: BUS,
    /// The bus the M5IOE1 sits on, held for the display's lifetime.
    _i2c: I2C,
    delay: DELAY,
    frame: BulkBox<[u16]>,
    orientation: DisplayOrientation,
}

/// Bindings carried only at the type level by the static factory.
type FactoryTypes<BUS, I2C, DELAY> = PhantomData<fn() -> (BUS, I2C, DELAY)>;

/// Static factory used by generated Board composition.
pub struct StopWatchDisplayImplementation<BUS, I2C, DELAY>(FactoryTypes<BUS, I2C, DELAY>);

impl<BUS, I2C, DELAY> PeripheralImplementation for StopWatchDisplayImplementation<BUS, I2C, DELAY>
where
    BUS: QuadSpiBus + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = StopWatchDisplayBindings<BUS, I2C, DELAY>;
    type Config = StopWatchDisplayConfig;
    type Peripheral = StopWatchDisplay<BUS, I2C, DELAY>;
    type Error = ErrorFor<BUS, I2C>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let StopWatchDisplayBindings {
            bus,
            mut i2c,
            mut delay,
        } = bindings;
        let mut expander = M5ioe1::new(&mut i2c, config.expander_address)
            .map_err(StopWatchDisplayError::Expander)?;
        reset_panel(&mut expander, &mut delay).map_err(StopWatchDisplayError::Expander)?;
        let frame =
            BulkBox::try_new_zeroed_slice(FRAME_PIXELS).map_err(StopWatchDisplayError::Memory)?;
        let mut display = StopWatchDisplay {
            bus,
            _i2c: i2c,
            delay,
            frame,
            orientation: config.orientation,
        };
        for command in M5STACK_STOPWATCH_INIT {
            display.command(command.command, command.data)?;
            if command.delay_ms != 0 {
                display.delay.delay_ms(u32::from(command.delay_ms));
            }
        }
        // Replace whatever the GRAM held with the black shadow frame.
        display.send_window(Window {
            x: 0,
            y: 0,
            width: SIDE,
            height: SIDE,
        })?;
        Ok(display)
    }
}

/// Wakes the expander, enables the panel supplies, and pulses the reset as
/// M5GFX does.
fn reset_panel<I2C: I2c, DELAY: DelayNs>(
    expander: &mut M5ioe1<'_, I2C>,
    delay: &mut DELAY,
) -> Result<(), ExpanderError<I2C::Error>> {
    expander.wake()?;
    expander.configure_output(RAIL_ENABLE, true)?;
    expander.configure_output(EXPANDER_IO4, true)?;
    expander.configure_output(OLED_RESET, true)?;
    delay.delay_ms(RESET_SETTLE_MS);
    expander.set_output(EXPANDER_IO4, false)?;
    expander.set_output(OLED_RESET, false)?;
    delay.delay_ms(RESET_LOW_MS);
    expander.set_output(EXPANDER_IO4, true)?;
    expander.set_output(OLED_RESET, true)?;
    delay.delay_ms(RESET_RECOVERY_MS);
    Ok(())
}

/// Maps a logical pixel to its index in the shadow framebuffer.
fn frame_index(orientation: DisplayOrientation, x: i32, y: i32) -> Option<(u16, u16)> {
    let x = u16::try_from(x).ok().filter(|x| *x < SIDE)?;
    let y = u16::try_from(y).ok().filter(|y| *y < SIDE)?;
    let last = SIDE - 1;
    Some(match orientation {
        DisplayOrientation::Deg0 => (x, y),
        DisplayOrientation::Deg90 => (last - y, x),
        DisplayOrientation::Deg180 => (last - x, last - y),
        DisplayOrientation::Deg270 => (y, last - x),
    })
}

/// Bounding box of the physical pixels touched by one drawing operation.
#[derive(Clone, Copy, Default)]
struct Dirty {
    area: Option<(u16, u16, u16, u16)>,
}

impl Dirty {
    fn include(&mut self, column: u16, row: u16) {
        self.area = Some(match self.area {
            None => (column, row, column, row),
            Some((left, top, right, bottom)) => (
                left.min(column),
                top.min(row),
                right.max(column),
                bottom.max(row),
            ),
        });
    }

    fn window(self) -> Option<Window> {
        let (left, top, right, bottom) = self.area?;
        Window::aligned(left, top, right - left + 1, bottom - top + 1, SIDE, SIDE)
    }
}

impl<BUS, I2C, DELAY> StopWatchDisplay<BUS, I2C, DELAY>
where
    BUS: QuadSpiBus,
    I2C: I2c,
{
    fn command(&mut self, command: u8, data: &[u8]) -> Result<(), ErrorFor<BUS, I2C>> {
        self.bus
            .write_single_line(COMMAND_WRITE_OPCODE, command_address(command), data)
            .map_err(StopWatchDisplayError::Bus)
    }

    fn store(&mut self, x: i32, y: i32, color: Rgb565, dirty: &mut Dirty) {
        if let Some((column, row)) = frame_index(self.orientation, x, y) {
            let index = usize::from(row) * ROW_PIXELS + usize::from(column);
            if let Some(pixel) = self.frame.get_mut(index) {
                *pixel = color.into_storage();
                dirty.include(column, row);
            }
        }
    }

    /// Sends one aligned window from the shadow framebuffer, row by row.
    fn send_window(&mut self, window: Window) -> Result<(), ErrorFor<BUS, I2C>> {
        let columns = window
            .column_parameters(COLUMN_OFFSET)
            .ok_or(StopWatchDisplayError::InvalidPixelRegion)?;
        let mut row_bytes = [0_u8; ROW_PIXELS * 2];
        let width = usize::from(window.width);
        let start = usize::from(window.x);
        for row in window.y..window.y + window.height {
            let rows = Window {
                y: row,
                height: 1,
                ..window
            }
            .row_parameters(ROW_OFFSET)
            .ok_or(StopWatchDisplayError::InvalidPixelRegion)?;
            let first = usize::from(row) * ROW_PIXELS + start;
            let source = self
                .frame
                .get(first..first + width)
                .ok_or(StopWatchDisplayError::InvalidPixelRegion)?;
            let bytes = &mut row_bytes[..width * 2];
            for (target, pixel) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(source) {
                *target = pixel.to_be_bytes();
            }
            self.command(COLUMN_ADDRESS, &columns)?;
            self.command(ROW_ADDRESS, &rows)?;
            self.bus
                .write(COLOR_WRITE_OPCODE, command_address(MEMORY_WRITE), bytes)
                .map_err(StopWatchDisplayError::Bus)?;
        }
        Ok(())
    }

    fn send_dirty(&mut self, dirty: Dirty) -> Result<(), ErrorFor<BUS, I2C>> {
        match dirty.window() {
            Some(window) => self.send_window(window),
            None => Ok(()),
        }
    }
}

impl<BUS, I2C, DELAY> OriginDimensions for StopWatchDisplay<BUS, I2C, DELAY> {
    fn size(&self) -> Size {
        Size::new(u32::from(SIDE), u32::from(SIDE))
    }
}

impl<BUS, I2C, DELAY> DrawTarget for StopWatchDisplay<BUS, I2C, DELAY>
where
    BUS: QuadSpiBus,
    I2C: I2c,
{
    type Color = Rgb565;
    type Error = ErrorFor<BUS, I2C>;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let mut dirty = Dirty::default();
        for Pixel(point, color) in pixels {
            self.store(point.x, point.y, color, &mut dirty);
        }
        self.send_dirty(dirty)
    }
}

impl<BUS, I2C, DELAY> Display for StopWatchDisplay<BUS, I2C, DELAY>
where
    BUS: QuadSpiBus,
    I2C: I2c,
    DELAY: DelayNs,
{
    type ControlError = ErrorFor<BUS, I2C>;
    type RenderError = ErrorFor<BUS, I2C>;

    fn descriptor(&self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Rgb565,
            DisplayTechnology::Oled,
            DisplayFeatures::BRIGHTNESS
                | DisplayFeatures::ORIENTATION
                | DisplayFeatures::SLEEP
                | DisplayFeatures::ROUND,
        )
    }

    fn draw_rgb565(&mut self, area: Rectangle, pixels: &[Rgb565]) -> Result<(), Self::RenderError> {
        let bounds = Rectangle::new(Default::default(), self.size());
        let expected = usize::try_from(area.size.width)
            .ok()
            .zip(usize::try_from(area.size.height).ok())
            .and_then(|(width, height)| width.checked_mul(height));
        if area.intersection(&bounds) != area || expected != Some(pixels.len()) {
            return Err(StopWatchDisplayError::InvalidPixelRegion);
        }
        let mut dirty = Dirty::default();
        let width = area.size.width as usize;
        for (row, line) in pixels.chunks_exact(width.max(1)).enumerate() {
            for (column, color) in line.iter().enumerate() {
                let x = area.top_left.x + column as i32;
                let y = area.top_left.y + row as i32;
                self.store(x, y, *color, &mut dirty);
            }
        }
        self.send_dirty(dirty)
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        Ok(())
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        match power {
            DisplayPower::On => {
                self.command(SLEEP_OUT, &[])?;
                self.delay.delay_ms(SLEEP_SETTLE_MS);
                self.command(DISPLAY_ON, &[])
            }
            DisplayPower::Sleep | DisplayPower::Off => {
                self.command(DISPLAY_OFF, &[])?;
                self.command(SLEEP_IN, &[])?;
                self.delay.delay_ms(SLEEP_SETTLE_MS);
                Ok(())
            }
        }
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        self.command(WRITE_BRIGHTNESS, &[brightness])
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
#[allow(clippy::expect_used)]
mod tests {
    use barracuda_peripheral::display::DisplayOrientation;

    use super::{Dirty, SIDE, frame_index};

    #[test]
    fn orientations_map_corners_onto_the_square_window() {
        let last = SIDE - 1;
        assert_eq!(frame_index(DisplayOrientation::Deg0, 0, 0), Some((0, 0)));
        assert_eq!(
            frame_index(DisplayOrientation::Deg90, 0, 0),
            Some((last, 0))
        );
        assert_eq!(
            frame_index(DisplayOrientation::Deg180, 0, 0),
            Some((last, last))
        );
        assert_eq!(
            frame_index(DisplayOrientation::Deg270, 0, 0),
            Some((0, last))
        );
        assert_eq!(frame_index(DisplayOrientation::Deg0, 468, 0), None);
        assert_eq!(frame_index(DisplayOrientation::Deg0, 0, -1), None);
    }

    #[test]
    fn dirty_pixels_widen_to_an_aligned_window() {
        let mut dirty = Dirty::default();
        assert!(dirty.window().is_none());
        dirty.include(5, 7);
        dirty.include(8, 7);
        let window = dirty.window().expect("window");
        assert_eq!(
            (window.x, window.y, window.width, window.height),
            (4, 6, 6, 2)
        );
    }
}

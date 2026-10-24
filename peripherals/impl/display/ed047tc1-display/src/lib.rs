//! Buffered 16-level display implementation of a directly driven ED047TC1 /
//! ET047TC1 4.7-inch 960 x 540 e-paper panel, as on the M5Stack PaperS3.
//!
//! The panel has no controller: the host clocks every line's source data over
//! an 8-bit parallel bus (a Platform [`ParallelOutput`]) and toggles the gate
//! and latch lines itself, once per waveform frame. The waveforms, per-pixel
//! progress, dithering, and line timing are M5Stack's M5GFX `Panel_EPD` and
//! `Bus_EPD` (see `barracuda-driver-ed047tc1`). A refresh runs frames until
//! every requested pixel has finished its waveform, then turns the panel
//! supply off again.
//!
//! M5GFX overlaps computing the next line with clocking out the current one
//! and drops the gate clock from the transfer-done interrupt. Here each line
//! is computed, clocked out by a blocking DMA transfer, and the gate clock
//! dropped as soon as the transfer returns, so the gate-clock high time per
//! line is the transfer time as in M5GFX, while the line period also includes
//! the computation. RGB565 input is reduced to 16 levels with M5GFX's 4 x 4
//! Bayer matrix.

#![no_std]

extern crate alloc;

use alloc::{vec, vec::Vec};
use core::{convert::Infallible, marker::PhantomData};

use barracuda_driver_ed047tc1::{
    self as ed047tc1, GRAY_FRAME_BYTES, GRAY_ROW_BYTES, HEIGHT, IDLE_ENTRY, LINE_BYTES,
    LINE_TRANSFER_BYTES, Mode, STEP_FRAME_ENTRIES, STEP_ROW_ENTRIES, ScanTiming, WIDTH,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    display::{
        Display, DisplayDescriptor, DisplayFeatures, DisplayOrientation, DisplayPower,
        DisplayTechnology, ParallelOutput, PixelFormat, RefreshMode, RefreshRequest,
    },
};
use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Point, Size},
    pixelcolor::{Gray4, GrayColor, Rgb565, RgbColor},
    primitives::Rectangle,
};
use embedded_hal::{delay::DelayNs, digital::OutputPin};

/// Optional operations of the panel.
pub const FEATURES: DisplayFeatures = DisplayFeatures::BUFFERED
    .union(DisplayFeatures::PARTIAL_REFRESH)
    .union(DisplayFeatures::SLEEP);

/// Pause between frames (M5GFX `vTaskDelay(1)`, one 1 ms tick).
const FRAME_GAP_MS: u32 = 1;
/// Frames after which a refresh gives up; the longest waveform with its
/// eraser is 36 frames.
const MAX_FRAMES: usize = 256;

/// M5GFX `Panel_EPD` `Bayer` matrix.
const BAYER: [i16; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/// Mounting of the panel on the Board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ed047tc1DisplayConfig {
    mounting: DisplayOrientation,
}

impl Ed047tc1DisplayConfig {
    /// Describes a panel turned clockwise by `mounting` from its scan
    /// direction to the Board's upright view.
    #[must_use]
    pub const fn new(mounting: DisplayOrientation) -> Self {
        Self { mounting }
    }

    /// Returns the upright drawing size.
    #[must_use]
    pub const fn size(self) -> Size {
        match self.mounting {
            DisplayOrientation::Deg90 | DisplayOrientation::Deg270 => {
                Size::new(HEIGHT as u32, WIDTH as u32)
            }
            DisplayOrientation::Deg0 | DisplayOrientation::Deg180 => {
                Size::new(WIDTH as u32, HEIGHT as u32)
            }
        }
    }

    /// Returns the descriptor of the display.
    #[must_use]
    pub const fn descriptor(self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Gray4,
            DisplayTechnology::Epaper,
            FEATURES,
        )
    }

    /// Maps an upright coordinate to the panel's scan coordinate, as M5GFX
    /// `_rotate_pos` does.
    #[must_use]
    pub const fn to_native(self, x: usize, y: usize) -> (usize, usize) {
        match self.mounting {
            DisplayOrientation::Deg0 => (x, y),
            DisplayOrientation::Deg90 => (WIDTH - 1 - y, x),
            DisplayOrientation::Deg180 => (WIDTH - 1 - x, HEIGHT - 1 - y),
            DisplayOrientation::Deg270 => (y, HEIGHT - 1 - x),
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct Ed047tc1Bindings<BUS, P, DELAY> {
    bus: BUS,
    timing: ScanTiming<P>,
    delay: DELAY,
}

impl<BUS, P: OutputPin, DELAY> Ed047tc1Bindings<BUS, P, DELAY> {
    /// Combines the parallel source bus with the gate start pulse, gate
    /// clock, source latch, source output enable, and panel supply outputs.
    #[must_use]
    pub const fn new(bus: BUS, spv: P, ckv: P, le: P, oe: P, pwr: P, delay: DELAY) -> Self {
        Self {
            bus,
            timing: ScanTiming::new(spv, ckv, le, oe, pwr),
            delay,
        }
    }
}

/// Initialization or display-control failure.
#[derive(Debug)]
pub enum Ed047tc1Error<BUS, PIN> {
    /// The parallel bus failed or is smaller than one line.
    Bus(BUS),
    /// The parallel bus cannot hold one line of source data.
    BusTooSmall,
    /// A timing output failed.
    Pin(PIN),
    /// A refresh did not finish within its frame limit.
    Timeout,
    /// The panel has no brightness control.
    BrightnessUnsupported,
    /// Only the Board's upright orientation is supported.
    OrientationUnsupported,
}

/// Error type of one concrete display.
pub type ErrorOf<BUS, P> =
    Ed047tc1Error<<BUS as ParallelOutput>::Error, <P as embedded_hal::digital::ErrorType>::Error>;

/// Initialized display.
pub struct Ed047tc1Display<BUS, P, DELAY> {
    bus: BUS,
    timing: ScanTiming<P>,
    delay: DELAY,
    config: Ed047tc1DisplayConfig,
    /// Native 4-bit framebuffer, two pixels per byte, first pixel high,
    /// `0` black.
    gray: Vec<u8>,
    /// Per-pixel-pair waveform progress (M5GFX `_step_framebuf`).
    steps: Vec<u16>,
    line: [u8; LINE_TRANSFER_BYTES],
}

/// Static implementation factory.
pub struct Ed047tc1DisplayImplementation<BUS, P, DELAY>(PhantomData<(BUS, P, DELAY)>);

impl<BUS, P, DELAY> PeripheralImplementation for Ed047tc1DisplayImplementation<BUS, P, DELAY>
where
    BUS: ParallelOutput + 'static,
    P: OutputPin + 'static,
    DELAY: DelayNs + 'static,
    BUS::Error: 'static,
    P::Error: 'static,
{
    type Bindings = Ed047tc1Bindings<BUS, P, DELAY>;
    type Config = Ed047tc1DisplayConfig;
    type Peripheral = Ed047tc1Display<BUS, P, DELAY>;
    type Error = ErrorOf<BUS, P>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        Ed047tc1Display::new(bindings, config)
    }
}

impl<BUS, P, DELAY> Ed047tc1Display<BUS, P, DELAY>
where
    BUS: ParallelOutput,
    P: OutputPin,
    DELAY: DelayNs,
{
    /// Takes the bindings with the panel supply off; nothing is drawn until
    /// the first flush.
    ///
    /// # Errors
    ///
    /// Returns [`Ed047tc1Error::BusTooSmall`] when the bus cannot hold a line.
    pub fn new(
        bindings: Ed047tc1Bindings<BUS, P, DELAY>,
        config: Ed047tc1DisplayConfig,
    ) -> Result<Self, ErrorOf<BUS, P>> {
        let Ed047tc1Bindings { bus, timing, delay } = bindings;
        if bus.capacity() < LINE_TRANSFER_BYTES {
            return Err(Ed047tc1Error::BusTooSmall);
        }
        Ok(Self {
            bus,
            timing,
            delay,
            config,
            gray: vec![0xFF; GRAY_FRAME_BYTES],
            steps: vec![IDLE_ENTRY; STEP_FRAME_ENTRIES],
            line: [0; LINE_TRANSFER_BYTES],
        })
    }

    fn set_native(&mut self, x: usize, y: usize, level: u8) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        if let Some(byte) = self.gray.get_mut(y * GRAY_ROW_BYTES + x / 2) {
            *byte = if x.is_multiple_of(2) {
                (*byte & 0x0F) | (level << 4)
            } else {
                (*byte & 0xF0) | (level & 0x0F)
            };
        }
    }

    fn native(&self, point: Point) -> Option<(usize, usize)> {
        let size = self.config.size();
        let x = usize::try_from(point.x).ok()?;
        let y = usize::try_from(point.y).ok()?;
        let inside = u32::try_from(x).is_ok_and(|x| x < size.width)
            && u32::try_from(y).is_ok_and(|y| y < size.height);
        inside.then(|| self.config.to_native(x, y))
    }

    /// Native rectangle `(x, y, width, height)` covering `region`.
    fn native_rect(&self, region: Option<Rectangle>) -> Option<(usize, usize, usize, usize)> {
        let upright = Rectangle::new(Point::zero(), self.config.size());
        let region = region.map_or(upright, |region| region.intersection(&upright));
        let bottom_right = region.bottom_right()?;
        let (ax, ay) = self.native(region.top_left)?;
        let (bx, by) = self.native(bottom_right)?;
        let (left, right) = (ax.min(bx) & !1, ax.max(bx) | 1);
        let (top, bottom) = (ay.min(by), ay.max(by));
        Some((left, top, right + 1 - left, bottom + 1 - top))
    }

    /// Runs waveform frames until every pixel is idle (M5GFX `task_update`).
    fn run_frames(&mut self) -> Result<(), ErrorOf<BUS, P>> {
        self.timing
            .power_on(&mut self.delay)
            .map_err(Ed047tc1Error::Pin)?;
        for _ in 0..MAX_FRAMES {
            let mut running = false;
            for (row, steps) in self
                .steps
                .as_chunks_mut::<STEP_ROW_ENTRIES>()
                .0
                .iter_mut()
                .enumerate()
            {
                let (data, _padding) = self.line.split_at_mut(LINE_BYTES);
                running |= ed047tc1::blit_row(steps, data);
                if row == 0 {
                    self.timing
                        .begin_frame(&mut self.delay)
                        .map_err(Ed047tc1Error::Pin)?;
                }
                self.timing.line_start().map_err(Ed047tc1Error::Pin)?;
                self.bus
                    .write_data(&self.line)
                    .map_err(Ed047tc1Error::Bus)?;
                self.timing.line_end().map_err(Ed047tc1Error::Pin)?;
            }
            self.delay.delay_ms(FRAME_GAP_MS);
            self.timing.end_frame().map_err(Ed047tc1Error::Pin)?;
            if !running {
                return self
                    .timing
                    .power_off(&mut self.delay)
                    .map_err(Ed047tc1Error::Pin);
            }
        }
        self.timing
            .power_off(&mut self.delay)
            .map_err(Ed047tc1Error::Pin)?;
        Err(Ed047tc1Error::Timeout)
    }
}

/// M5GFX's 16-level reduction of one RGB565 pixel at native `(x, y)`:
/// `((R + 2G + B) / 4 + bayer - 8) >> 4`, `0` black to `15` white.
#[must_use]
pub fn dithered_level(pixel: Rgb565, x: usize, y: usize) -> u8 {
    let expand = |value: u8, max: u16| i16::try_from(u16::from(value) * 255 / max).unwrap_or(255);
    let gray = (expand(pixel.r(), 31) + 2 * expand(pixel.g(), 63) + expand(pixel.b(), 31)) / 4;
    let bayer = BAYER[(y & 3) << 2 | (x & 3)];
    let level = (gray + bayer - 8).max(0) >> 4;
    u8::try_from(level.min(15)).unwrap_or(15)
}

/// M5GFX's waveform for each refresh mode: quality, text, fast.
#[must_use]
pub const fn mode_for(mode: RefreshMode) -> Mode {
    match mode {
        RefreshMode::Automatic | RefreshMode::Full => Mode::Quality,
        RefreshMode::Partial => Mode::Text,
        RefreshMode::Fast => Mode::Fast,
    }
}

impl<BUS, P, DELAY> OriginDimensions for Ed047tc1Display<BUS, P, DELAY> {
    fn size(&self) -> Size {
        self.config.size()
    }
}

impl<BUS, P, DELAY> DrawTarget for Ed047tc1Display<BUS, P, DELAY>
where
    BUS: ParallelOutput,
    P: OutputPin,
    DELAY: DelayNs,
{
    type Color = Gray4;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if let Some((x, y)) = self.native(point) {
                self.set_native(x, y, color.luma());
            }
        }
        Ok(())
    }
}

impl<BUS, P, DELAY> Display for Ed047tc1Display<BUS, P, DELAY>
where
    BUS: ParallelOutput,
    P: OutputPin,
    DELAY: DelayNs,
{
    type ControlError = ErrorOf<BUS, P>;
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
            if let Some((x, y)) = self.native(point) {
                self.set_native(x, y, dithered_level(*pixel, x, y));
            }
            offset = offset.saturating_add(1);
        }
        Ok(())
    }

    async fn flush(&mut self, request: RefreshRequest) -> Result<(), Self::ControlError> {
        let Some((x, y, width, height)) = self.native_rect(request.region()) else {
            return Ok(());
        };
        ed047tc1::request_update(
            &self.gray,
            &mut self.steps,
            x,
            y,
            width,
            height,
            mode_for(request.mode()),
        );
        self.run_frames()
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        // The supply is only on while a refresh runs (M5GFX `setSleep` and
        // `setPowerSave` switch the same supply).
        if power != DisplayPower::On {
            self.timing
                .power_off(&mut self.delay)
                .map_err(Ed047tc1Error::Pin)?;
        }
        Ok(())
    }

    async fn set_brightness(&mut self, _brightness: u8) -> Result<(), Self::ControlError> {
        Err(Ed047tc1Error::BrightnessUnsupported)
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        if orientation == DisplayOrientation::Deg0 {
            Ok(())
        } else {
            Err(Ed047tc1Error::OrientationUnsupported)
        }
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(())
    }
}

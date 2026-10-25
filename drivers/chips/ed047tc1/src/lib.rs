//! Low-level Driver for the ED047TC1 / ET047TC1 4.7-inch 960 x 540 e-paper
//! panel, driven directly over its source-driver data bus and gate/source
//! timing lines (no controller chip).
//!
//! Everything here is a port of M5Stack's M5GFX `Panel_EPD` and `Bus_EPD`
//! (the M5PaperS3 path):
//!
//! - **Waveforms.** Each waveform is a list of frames; each frame gives, for
//!   each of the 16 gray levels, one of "to black", "to white", "no
//!   operation", or "end". [`TWO_PIXEL_LUT`] expands every frame for every
//!   pair of 4-bit pixels into the 4-bit source code of the pair, exactly as
//!   M5GFX's `_lut_2pixel`.
//! - **Per-pixel progress.** Every two pixels own two 16-bit entries: the
//!   running step (a LUT row and the pair's gray value; negative when idle)
//!   and the reserved next one. Quality and text updates run the short
//!   "eraser" waveform from the old gray value first, then the target
//!   waveform; fast updates start the target waveform at once. A new request
//!   for a pixel still erasing only replaces its reservation.
//! - **Frames.** [`blit_row`] advances every running entry of one row by one
//!   frame and produces the row's source bytes, four pixels per byte, first
//!   pixel in the top bits. Frames repeat until no entry runs.
//! - **Timing.** [`ScanTiming`] drives the gate start pulse (`SPV`), gate
//!   clock (`CKV`), source latch (`LE`), output enable (`OE`), and panel supply
//!   (`PWR`) with M5GFX's delays; the source data and its clock and start
//!   pulse (`CL`, `SPH`) come from a parallel bus outside this crate.
//!
//! The source code per pixel is two bits: `01` drives toward black, `10`
//! toward white, `00` and `11` leave the pixel alone.

#![no_std]

use embedded_hal::{delay::DelayNs, digital::OutputPin};

/// Native panel width (source direction) in pixels.
pub const WIDTH: usize = 960;
/// Native panel height (gate direction) in lines.
pub const HEIGHT: usize = 540;
/// Bytes of one packed 4-bit row of the gray framebuffer.
pub const GRAY_ROW_BYTES: usize = WIDTH / 2;
/// Bytes of the whole packed 4-bit gray framebuffer.
pub const GRAY_FRAME_BYTES: usize = GRAY_ROW_BYTES * HEIGHT;
/// Progress entries of one row (two per pixel pair).
pub const STEP_ROW_ENTRIES: usize = WIDTH;
/// Progress entries of the whole panel.
pub const STEP_FRAME_ENTRIES: usize = STEP_ROW_ENTRIES * HEIGHT;
/// Source bytes of one line, four pixels per byte.
pub const LINE_BYTES: usize = WIDTH / 4;
/// Zero bytes M5GFX appends to every line (`line_padding`).
pub const LINE_PADDING: usize = 8;
/// Bytes clocked out per line.
pub const LINE_TRANSFER_BYTES: usize = LINE_BYTES + LINE_PADDING;
/// Source-bus clock M5GFX uses on the M5PaperS3 (`bus_speed`).
pub const BUS_CLOCK_HZ: u32 = 16_000_000;
/// Initial progress entry: idle, gray level 8 for both pixels (M5GFX
/// `memset(_step_framebuf, 0x88, ...)`).
pub const IDLE_ENTRY: u16 = 0x8888;

const IDLE: u16 = 0x8000;

/// Packs one frame row: the 2-bit action of each of the 16 gray levels,
/// level 0 (black) in the lowest bits. `1` to black, `2` to white, `3` no
/// operation, `0` end of waveform.
const fn frame(levels: [u32; 16]) -> u32 {
    let mut packed = 0;
    let mut level = 0;
    while level < 16 {
        packed |= levels[level] << (level * 2);
        level += 1;
    }
    packed
}

const NOP: u32 = u32::MAX;
const END: u32 = 0;

/// M5GFX `lut_quality`.
pub const WAVEFORM_QUALITY: &[u32] = &[
    frame([1, 1, 1, 1, 1, 1, 1, 2, 1, 2, 2, 1, 1, 1, 1, 1]),
    frame([2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2]),
    frame([2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2]),
    frame([1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]),
    frame([1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]),
    frame([1, 1, 1, 1, 1, 1, 1, 1, 2, 1, 1, 1, 1, 1, 1, 1]),
    frame([1, 1, 2, 2, 1, 1, 1, 2, 1, 2, 1, 1, 1, 1, 1, 3]),
    frame([1, 1, 1, 1, 1, 2, 1, 1, 2, 2, 1, 2, 1, 2, 2, 2]),
    frame([1, 1, 3, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1, 1, 2, 2]),
    frame([3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2]),
    frame([3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2]),
    frame([1, 1, 1, 1, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2]),
    frame([1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 3, 2, 2, 2, 2]),
    frame([3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3]),
    frame([3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 2, 2, 3]),
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    END,
];

/// M5GFX `lut_text`.
pub const WAVEFORM_TEXT: &[u32] = &[
    frame([2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1]),
    frame([2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1]),
    frame([2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1]),
    frame([2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 1]),
    frame([2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 1]),
    frame([1, 2, 2, 1, 1, 1, 1, 1, 3, 3, 1, 1, 3, 3, 1, 2]),
    frame([1, 3, 3, 1, 1, 1, 1, 3, 3, 1, 1, 1, 1, 3, 1, 2]),
    frame([1, 3, 3, 1, 2, 2, 1, 1, 1, 1, 2, 1, 1, 1, 1, 2]),
    frame([3, 1, 3, 2, 2, 2, 1, 1, 1, 2, 2, 1, 1, 1, 2, 3]),
    frame([1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2]),
    frame([1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2]),
    frame([1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 3, 3, 3, 2]),
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    NOP,
    END,
];

/// M5GFX `lut_fast`.
pub const WAVEFORM_FAST: &[u32] = &[
    frame([2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1]),
    frame([2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1]),
    frame([1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]),
    frame([1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]),
    frame([1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]),
    frame([1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]),
    frame([1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]),
    frame([1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]),
    NOP,
    END,
];

/// M5GFX `lut_fastest`.
pub const WAVEFORM_FASTEST: &[u32] = &[
    frame([2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1]),
    frame([1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]),
    frame([1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]),
    frame([1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]),
    frame([1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2]),
    NOP,
    END,
];

/// M5GFX `lut_eraser`: shifts the previous gray toward the middle before a
/// quality or text update draws the new one.
pub const WAVEFORM_ERASER: &[u32] = &[
    frame([2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 1, 1]),
    frame([2, 2, 3, 3, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]),
    NOP,
    END,
];

/// Total frames of all waveforms, eraser first.
pub const TOTAL_STEPS: usize = WAVEFORM_ERASER.len()
    + WAVEFORM_QUALITY.len()
    + WAVEFORM_TEXT.len()
    + WAVEFORM_FAST.len()
    + WAVEFORM_FASTEST.len();

/// Update waveform of one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Erase, then the 16-level quality waveform.
    Quality,
    /// Erase, then the text waveform; non-white pixels are always redrawn.
    Text,
    /// Black and white only, no erase.
    Fast,
    /// Black and white only, shortest waveform, no erase.
    Fastest,
}

impl Mode {
    /// First LUT row of this mode's waveform in [`TWO_PIXEL_LUT`].
    #[must_use]
    pub const fn first_step(self) -> usize {
        let eraser = WAVEFORM_ERASER.len();
        match self {
            Self::Quality => eraser,
            Self::Text => eraser + WAVEFORM_QUALITY.len(),
            Self::Fast => eraser + WAVEFORM_QUALITY.len() + WAVEFORM_TEXT.len(),
            Self::Fastest => {
                eraser + WAVEFORM_QUALITY.len() + WAVEFORM_TEXT.len() + WAVEFORM_FAST.len()
            }
        }
    }
}

const fn expand_luts() -> [u8; TOTAL_STEPS * 256] {
    let mut table = [0_u8; TOTAL_STEPS * 256];
    let waveforms = [
        WAVEFORM_ERASER,
        WAVEFORM_QUALITY,
        WAVEFORM_TEXT,
        WAVEFORM_FAST,
        WAVEFORM_FASTEST,
    ];
    let mut index = 0;
    let mut waveform = 0;
    while waveform < waveforms.len() {
        let frames = waveforms[waveform];
        let mut step = 0;
        while step < frames.len() {
            let row = frames[step];
            let mut pair = 0;
            while pair < 256 {
                let high = (row >> ((pair >> 4) * 2)) & 3;
                let low = (row >> ((pair & 15) * 2)) & 3;
                table[index] = ((high << 2) | low) as u8;
                index += 1;
                pair += 1;
            }
            step += 1;
        }
        waveform += 1;
    }
    table
}

/// Source code of every pixel pair for every waveform frame: entry
/// `step * 256 + pair` holds the first pixel's two bits above the second's.
pub static TWO_PIXEL_LUT: [u8; TOTAL_STEPS * 256] = expand_luts();

/// Requests an update of the native rectangle `x..x + width`, `y..y + height`
/// from the 4-bit gray framebuffer `gray` (two pixels per byte, first pixel
/// in the high nibble, `0` black, `15` white) into the progress entries
/// `steps`. `x` and `width` are rounded out to whole pixel pairs.
///
/// Fast modes take each pixel as black below level 8 and white from it,
/// since their waveforms only move the two extremes.
pub fn request_update(
    gray: &[u8],
    steps: &mut [u16],
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    mode: Mode,
) {
    let first_pair = x / 2;
    let last_pair = (x.saturating_add(width).saturating_add(1) / 2).min(GRAY_ROW_BYTES);
    let last_row = y.saturating_add(height).min(HEIGHT);
    let offset = u16::try_from(mode.first_step() << 8).unwrap_or(0);
    for row in y..last_row {
        for pair in first_pair..last_pair {
            let index = row * GRAY_ROW_BYTES + pair;
            let (Some(&value), Some(entries)) =
                (gray.get(index), steps.get_mut(index * 2..index * 2 + 2))
            else {
                continue;
            };
            match mode {
                Mode::Fast | Mode::Fastest => {
                    step_fast(offset + u16::from(threshold(value)) + IDLE, entries);
                }
                Mode::Text => step_text(offset + u16::from(value), entries, offset | 0x00FF),
                Mode::Quality => step_quality(offset + u16::from(value), entries),
            }
        }
    }
}

const fn threshold(pair: u8) -> u8 {
    let high = if pair >> 4 >= 8 { 0xF0 } else { 0x00 };
    let low = if pair & 0x0F >= 8 { 0x0F } else { 0x00 };
    high | low
}

/// M5GFX `step_fast`: start the waveform at once unless already requested.
fn step_fast(start: u16, entries: &mut [u16]) {
    if entries[1] != start {
        entries[1] = start;
        entries[0] = start.wrapping_sub(IDLE);
    }
}

/// M5GFX `step_quality`: reserve the target and erase from the old gray,
/// unless already erasing.
fn step_quality(start: u16, entries: &mut [u16]) {
    if entries[1] != start {
        let current = entries[0];
        entries[1] = start;
        if current >= eraser_end() {
            entries[0] = current & 0x00FF;
        }
    }
}

/// M5GFX `step_text`: as quality, but pixels that are not white are redrawn
/// even when unchanged.
fn step_text(start: u16, entries: &mut [u16], white: u16) {
    let requested = entries[1] & 0x7FFF;
    if white != requested || requested != start {
        let current = entries[0];
        entries[1] = start;
        if current >= eraser_end() {
            entries[0] = current & 0x00FF;
        }
    }
}

const fn eraser_end() -> u16 {
    (WAVEFORM_ERASER.len() << 8) as u16
}

/// Advances one row of progress entries by one frame and writes the row's
/// source bytes into `line` (four pixels per byte, first pixel in bits 7..6).
/// Returns whether any entry was running.
pub fn blit_row(steps: &mut [u16], line: &mut [u8]) -> bool {
    let mut running = false;
    for (pairs, out) in steps.as_chunks_mut::<4>().0.iter_mut().zip(line.iter_mut()) {
        let first = advance(&mut pairs[..2]);
        let second = advance(&mut pairs[2..]);
        running |= first.is_some() || second.is_some();
        *out = first.unwrap_or(0) << 4 | second.unwrap_or(0);
    }
    running
}

/// Advances one pair's entries; `None` when idle.
fn advance(entries: &mut [u16]) -> Option<u8> {
    let current = entries[0];
    if current & IDLE != 0 {
        return None;
    }
    let code = TWO_PIXEL_LUT
        .get(usize::from(current))
        .copied()
        .unwrap_or(0);
    if code == 0 {
        // End of this waveform: continue with the reservation and mark it
        // consumed.
        entries[0] = entries[1];
        entries[1] |= IDLE;
    } else {
        entries[0] = current.wrapping_add(256);
    }
    Some(code)
}

/// Gate and source timing lines of the panel, all one output type.
pub struct ScanTiming<P> {
    spv: P,
    ckv: P,
    le: P,
    oe: P,
    pwr: P,
    powered: bool,
}

impl<P: OutputPin> ScanTiming<P> {
    /// Takes the gate start pulse, gate clock, source latch, output enable,
    /// and panel supply outputs.
    pub const fn new(spv: P, ckv: P, le: P, oe: P, pwr: P) -> Self {
        Self {
            spv,
            ckv,
            le,
            oe,
            pwr,
            powered: false,
        }
    }

    /// Returns whether the panel supply is on.
    #[must_use]
    pub const fn is_powered(&self) -> bool {
        self.powered
    }

    /// M5GFX `Bus_EPD::powerControl(true)`: OE, PWR, then SPV high.
    ///
    /// # Errors
    ///
    /// Returns the pin error.
    pub fn power_on(&mut self, delay: &mut impl DelayNs) -> Result<(), P::Error> {
        if self.powered {
            return Ok(());
        }
        self.oe.set_high()?;
        delay.delay_us(100);
        self.pwr.set_high()?;
        delay.delay_us(100);
        self.spv.set_high()?;
        delay.delay_ms(1);
        self.powered = true;
        Ok(())
    }

    /// M5GFX `Bus_EPD::powerControl(false)`: PWR, OE, then SPV low.
    ///
    /// # Errors
    ///
    /// Returns the pin error.
    pub fn power_off(&mut self, delay: &mut impl DelayNs) -> Result<(), P::Error> {
        if !self.powered {
            return Ok(());
        }
        delay.delay_ms(1);
        self.pwr.set_low()?;
        delay.delay_us(10);
        self.oe.set_low()?;
        delay.delay_us(100);
        self.spv.set_low()?;
        self.powered = false;
        Ok(())
    }

    /// M5GFX `Bus_EPD::beginTransaction`: the gate start pulse, then three
    /// gate clocks.
    ///
    /// # Errors
    ///
    /// Returns the pin error.
    pub fn begin_frame(&mut self, delay: &mut impl DelayNs) -> Result<(), P::Error> {
        self.spv.set_low()?;
        delay.delay_us(1);
        self.ckv.set_low()?;
        delay.delay_us(3);
        self.ckv.set_high()?;
        delay.delay_us(1);
        self.spv.set_high()?;
        for _ in 0..3 {
            delay.delay_us(3);
            self.ckv.set_low()?;
            delay.delay_us(3);
            self.ckv.set_high()?;
        }
        Ok(())
    }

    /// M5GFX `Bus_EPD::writeScanLine` before the data: latch low, gate clock
    /// high.
    ///
    /// # Errors
    ///
    /// Returns the pin error.
    pub fn line_start(&mut self) -> Result<(), P::Error> {
        self.le.set_low()?;
        self.ckv.set_high()
    }

    /// M5GFX `notify_line_done` after the data: gate clock low, latch high.
    ///
    /// # Errors
    ///
    /// Returns the pin error.
    pub fn line_end(&mut self) -> Result<(), P::Error> {
        self.ckv.set_low()?;
        self.le.set_high()
    }

    /// M5GFX `Bus_EPD::endTransaction`: latch low, gate clock high.
    ///
    /// # Errors
    ///
    /// Returns the pin error.
    pub fn end_frame(&mut self) -> Result<(), P::Error> {
        self.le.set_low()?;
        self.ckv.set_high()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    extern crate std;

    use core::{cell::RefCell, convert::Infallible};
    use std::{vec, vec::Vec};

    use embedded_hal::{
        delay::DelayNs,
        digital::{ErrorType, OutputPin},
    };

    use super::{
        GRAY_ROW_BYTES, IDLE_ENTRY, LINE_BYTES, Mode, STEP_ROW_ENTRIES, ScanTiming, TOTAL_STEPS,
        TWO_PIXEL_LUT, WAVEFORM_ERASER, WAVEFORM_FAST, WAVEFORM_QUALITY, blit_row, request_update,
    };

    /// Runs frames over one row until idle; returns each frame's first byte.
    fn run_row(steps: &mut [u16]) -> Vec<u8> {
        let mut line = [0_u8; LINE_BYTES];
        let mut frames = Vec::new();
        while blit_row(steps, &mut line) {
            frames.push(line[0]);
            assert!(frames.len() < 200, "waveform never ended");
        }
        frames
    }

    #[test]
    fn lut_expansion_matches_m5gfx_layout() {
        assert_eq!(TOTAL_STEPS, 4 + 32 + 32 + 10 + 7);
        // Quality frame 0: level 0 -> black (1), level 7 -> white (2).
        let base = Mode::Quality.first_step() * 256;
        assert_eq!(TWO_PIXEL_LUT[base], 0b0101);
        assert_eq!(TWO_PIXEL_LUT[base + 0x07], 0b0110);
        assert_eq!(TWO_PIXEL_LUT[base + 0x70], 0b1001);
        // A no-operation frame drives nothing (3 per pixel).
        assert_eq!(TWO_PIXEL_LUT[(base + 15 * 256) + 0x5A], 0b1111);
        // The terminator ends both pixels.
        assert_eq!(
            TWO_PIXEL_LUT[(Mode::Quality.first_step() + 31) * 256 + 0x33],
            0
        );
    }

    #[test]
    fn quality_update_erases_then_draws_and_returns_idle() {
        let mut gray = vec![0xFF_u8; GRAY_ROW_BYTES];
        gray[0] = 0x00;
        let mut steps = vec![IDLE_ENTRY; STEP_ROW_ENTRIES];
        request_update(&gray, &mut steps, 0, 0, 4, 1, Mode::Quality);
        // Eraser from the initial gray 8 runs first.
        assert_eq!(steps[0], 0x0088);
        assert_eq!(
            steps[1],
            u16::try_from(Mode::Quality.first_step() << 8).expect("offset")
        );
        let frames = run_row(&mut steps);
        // Eraser (2 driving + 1 no-op + end) then quality (31 + end).
        assert_eq!(frames.len(), WAVEFORM_ERASER.len() + WAVEFORM_QUALITY.len());
        // The first eraser frame takes gray 8 to white for both pixels of
        // pair 0 and of pair 1.
        assert_eq!(frames[0], 0b1010_1010);
        // The first quality frame drives black (0) to black, white (15) to
        // black as well (M5GFX quality frame 0, level 15 = 1).
        assert_eq!(frames[WAVEFORM_ERASER.len()], 0b0101_0101);
        assert!(steps.iter().all(|entry| entry & 0x8000 != 0));
        // The settled entries keep the drawn gray for the next erase.
        assert_eq!(steps[0] & 0xFF, 0x00);
        assert_eq!(steps[2] & 0xFF, 0xFF);
    }

    #[test]
    fn fast_update_skips_the_eraser_and_thresholds_grays() {
        let mut gray = vec![0xFF_u8; GRAY_ROW_BYTES];
        gray[0] = 0x39;
        let mut steps = vec![IDLE_ENTRY; STEP_ROW_ENTRIES];
        request_update(&gray, &mut steps, 0, 0, 2, 1, Mode::Fast);
        assert_eq!(steps[0] & 0xFF, 0x0F);
        let frames = run_row(&mut steps);
        assert_eq!(frames.len(), WAVEFORM_FAST.len());
        // Fast frame 0: black -> white (2), white -> black (1).
        assert_eq!(frames[0] >> 4, 0b1001);
        // A repeated identical fast request changes nothing.
        request_update(&gray, &mut steps, 0, 0, 2, 1, Mode::Fast);
        assert!(run_row(&mut steps).is_empty());
    }

    #[test]
    fn repeated_request_during_erase_only_moves_the_reservation() {
        let gray = vec![0x00_u8; GRAY_ROW_BYTES];
        let mut steps = vec![IDLE_ENTRY; STEP_ROW_ENTRIES];
        request_update(&gray, &mut steps, 0, 0, 2, 1, Mode::Quality);
        let mut line = [0_u8; LINE_BYTES];
        assert!(blit_row(&mut steps, &mut line));
        let erasing = steps[0];
        request_update(&gray, &mut steps, 0, 0, 2, 1, Mode::Text);
        assert_eq!(steps[0], erasing);
        assert_eq!(
            steps[1] >> 8,
            u16::try_from(Mode::Text.first_step()).expect("step")
        );
    }

    #[derive(Default)]
    struct Log(RefCell<Vec<(&'static str, bool)>>);

    struct Pin<'a> {
        name: &'static str,
        log: &'a Log,
    }

    impl ErrorType for Pin<'_> {
        type Error = Infallible;
    }

    impl OutputPin for Pin<'_> {
        fn set_low(&mut self) -> Result<(), Infallible> {
            self.log.0.borrow_mut().push((self.name, false));
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Infallible> {
            self.log.0.borrow_mut().push((self.name, true));
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay {
        us: u32,
    }

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.us += ns / 1_000;
        }
    }

    #[test]
    fn timing_follows_bus_epd() {
        let log = Log::default();
        let pin = |name| Pin { name, log: &log };
        let mut timing = ScanTiming::new(pin("spv"), pin("ckv"), pin("le"), pin("oe"), pin("pwr"));
        let mut delay = Delay::default();
        timing.power_on(&mut delay).expect("power");
        timing.power_on(&mut delay).expect("idempotent");
        assert_eq!(
            log.0.borrow().as_slice(),
            &[("oe", true), ("pwr", true), ("spv", true)]
        );
        assert_eq!(delay.us, 1_200);
        log.0.borrow_mut().clear();
        timing.begin_frame(&mut delay).expect("frame");
        timing.line_start().expect("start");
        timing.line_end().expect("end");
        timing.end_frame().expect("end frame");
        let events = log.0.borrow();
        assert_eq!(
            &events[..4],
            &[("spv", false), ("ckv", false), ("ckv", true), ("spv", true)]
        );
        assert_eq!(events.len(), 4 + 6 + 2 + 2 + 2);
        assert_eq!(&events[10..12], &[("le", false), ("ckv", true)]);
        assert_eq!(&events[12..14], &[("ckv", false), ("le", true)]);
    }
}

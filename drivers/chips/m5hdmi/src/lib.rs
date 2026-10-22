//! M5Stack HDMI FPGA driver: the Gowin GW1NR-9 behind the LT8618SX on the
//! ATOM Display, the Module Display, and the Unit PoE-P4 HDMI.
//!
//! The FPGA holds no configuration of its own: M5GFX (`Panel_M5HDMI`,
//! M5GFX 0.2.34) loads M5Stack's design into its SRAM over JTAG at power-up
//! and skips the load when the FPGA already answers, after a warm reset.
//! The design then takes drawing commands over SPI (mode 3) and scans its
//! frame memory out as RGB video. This crate ports, in the chip's
//! vocabulary:
//!
//! - [`load_sram`]: M5GFX `LOAD_FPGA`, the Gowin JTAG sequence that erases
//!   the SRAM and streams [`BITSTREAM_RLE`] into it, over a [`JtagPort`];
//! - the SPI command encodings (`Panel_M5HDMI::CMD_*`): windows, raw pixel
//!   streams, filled rectangles, pixels, video timing, pixel clock, and
//!   scaling;
//! - [`VideoMode::new`]: M5GFX `config_resolution`, `getPllParams`, and
//!   `_init_resolution`, which derive the output size, scale, PLL dividers,
//!   and blanking from a logical size and refresh rate.
//!
//! [`BITSTREAM_RLE`] is M5Stack's configuration as M5GFX ships it
//! (`fs_bitstream_rle`, MIT-licensed): records of a count byte n > 0 and a
//! byte repeated n times, or a zero, a count n, and n bytes copied.

#![no_std]

/// M5Stack's FPGA configuration, run-length encoded as M5GFX 0.2.34 ships it.
pub static BITSTREAM_RLE: &[u8] = include_bytes!("../bitstream/m5hdmi-fs.rle");

/// The FPGA PLL's input clock (M5GFX `base_clock`).
pub const BASE_CLOCK_HZ: u32 = 74_250_000;

/// What `READ_ID` answers first: "HD".
pub const ID_PREFIX: [u8; 2] = *b"HD";

/// Command opcodes (M5GFX `Panel_M5HDMI::CMD_*`).
pub mod command {
    /// No operation; also what the host clocks out while reading.
    pub const NOP: u8 = 0x00;
    /// Answers `H`, `D`, major, and minor version.
    pub const READ_ID: u8 = 0x04;
    /// Display scaling: x and y factor, logical width and height.
    pub const SCREEN_SCALING: u8 = 0x18;
    /// Column range of the following raw write.
    pub const CASET: u8 = 0x2a;
    /// Row range of the following raw write.
    pub const RASET: u8 = 0x2b;
    /// Raw RGB565 pixels, big-endian, until chip select rises.
    pub const WRITE_RAW_16: u8 = 0x42;
    /// One RGB565 pixel.
    pub const DRAWPIXEL_16: u8 = 0x62;
    /// One RGB565-filled rectangle.
    pub const FILLRECT_16: u8 = 0x6a;
    /// Vertical video timing.
    pub const VIDEO_TIMING_V: u8 = 0xb0;
    /// Horizontal video timing.
    pub const VIDEO_TIMING_H: u8 = 0xb1;
    /// Pixel-clock PLL dividers.
    pub const VIDEO_CLOCK: u8 = 0xb2;
}

/// Gowin JTAG instructions M5GFX uses.
mod instruction {
    pub const ISC_NOOP: u8 = 0x02;
    pub const ISC_ERASE: u8 = 0x05;
    pub const ERASE_DONE: u8 = 0x09;
    pub const ISC_ENABLE: u8 = 0x15;
    pub const FAST_PROGRAM: u8 = 0x17;
    pub const ISC_DISABLE: u8 = 0x3a;
}

/// How long M5GFX lets the FPGA come out of power-on before configuring it:
/// it polls the status register once a millisecond, at most 128 times.
pub const POWER_ON_WAIT_MS: u32 = 128;
/// M5GFX `JTAG_DUMMY_CLOCK(4)` after the erase and after the disable.
const ERASE_CLOCK_MS: u32 = 4;

/// The four JTAG lines as the loader drives them.
///
/// TCK clocks TDI in on its rising edge and TDO out on its falling edge.
/// Shifts send each byte's most significant bit first; TMS keeps the level
/// last set.
pub trait JtagPort {
    /// Transport failure.
    type Error;

    /// Sets TMS for the following clocks.
    fn set_tms(&mut self, high: bool) -> Result<(), Self::Error>;

    /// Clocks `cycles` times; TDI does not matter.
    fn clock(&mut self, cycles: u8) -> Result<(), Self::Error>;

    /// Clocks out the low `bits` bits of `value`, 1 through 8, most
    /// significant first.
    fn shift_bits(&mut self, value: u8, bits: u8) -> Result<(), Self::Error>;

    /// Clocks out whole bytes.
    fn shift_bytes(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;

    /// Keeps clocking for at least `ms` milliseconds.
    fn clock_for_ms(&mut self, ms: u32) -> Result<(), Self::Error>;

    /// Waits `ms` milliseconds without clocking.
    fn delay_ms(&mut self, ms: u32) -> Result<(), Self::Error>;
}

/// SRAM configuration failure.
#[derive(Debug, PartialEq, Eq)]
pub enum LoadError<E> {
    /// The JTAG port failed.
    Port(E),
    /// The bitstream's run-length records are truncated.
    MalformedBitstream,
}

impl<E> From<E> for LoadError<E> {
    fn from(error: E) -> Self {
        Self::Port(error)
    }
}

/// A run-length record runs past the end of the bitstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MalformedBitstream;

/// One record of the run-length-encoded bitstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run<'a> {
    /// `byte` repeated `count` times.
    Repeat {
        /// The repeated byte.
        byte: u8,
        /// How many times, 1 through 255.
        count: u8,
    },
    /// Bytes copied as they are.
    Literal(&'a [u8]),
}

/// Iterates the records of a run-length-encoded bitstream.
#[derive(Clone, Debug)]
pub struct Runs<'a> {
    rest: &'a [u8],
}

impl<'a> Runs<'a> {
    /// Iterates `rle`'s records.
    #[must_use]
    pub const fn new(rle: &'a [u8]) -> Self {
        Self { rest: rle }
    }
}

impl<'a> Iterator for Runs<'a> {
    type Item = Result<Run<'a>, MalformedBitstream>;

    fn next(&mut self) -> Option<Self::Item> {
        let (&count, rest) = self.rest.split_first()?;
        let record = if count > 0 {
            rest.split_first()
                .map(|(&byte, rest)| (Run::Repeat { byte, count }, rest))
        } else {
            rest.split_first().and_then(|(&length, rest)| {
                (usize::from(length) <= rest.len()).then(|| {
                    let (bytes, rest) = rest.split_at(usize::from(length));
                    (Run::Literal(bytes), rest)
                })
            })
        };
        match record {
            Some((run, rest)) => {
                self.rest = rest;
                Some(Ok(run))
            }
            None => {
                self.rest = &[];
                Some(Err(MalformedBitstream))
            }
        }
    }
}

/// Returns the number of bytes `rle` decodes to.
///
/// # Errors
///
/// Returns [`MalformedBitstream`] for truncated records.
pub fn decoded_len(rle: &[u8]) -> Result<usize, MalformedBitstream> {
    Runs::new(rle).try_fold(0usize, |total, run| {
        let length = match run? {
            Run::Repeat { count, .. } => usize::from(count),
            Run::Literal(bytes) => bytes.len(),
        };
        Ok(total.saturating_add(length))
    })
}

/// Configures the FPGA's SRAM with `rle` (M5GFX `LOAD_FPGA`).
///
/// The sequence: wait out power-on; reset the TAP to Run-Test/Idle; enable
/// configuration, erase the SRAM, and clock 4 ms; finish the erase and
/// disable configuration, and clock 4 ms; enable configuration and start a
/// fast program; shift the decoded bitstream through Shift-DR, raising TMS
/// on its last bit; return to Run-Test/Idle; and disable configuration,
/// which starts the design. M5GFX polls the status register during the
/// power-on wait; this loader waits the poll's whole limit instead.
///
/// # Errors
///
/// Returns [`LoadError::Port`] when the port fails and
/// [`LoadError::MalformedBitstream`] for truncated records, after which the
/// FPGA is left unconfigured.
pub fn load_sram<P: JtagPort>(port: &mut P, rle: &[u8]) -> Result<(), LoadError<P::Error>> {
    port.set_tms(false)?;
    port.delay_ms(POWER_ON_WAIT_MS)?;
    // Test-Logic-Reset from any state, then Run-Test/Idle.
    move_tap(port, 8, 2)?;

    write_instruction(port, instruction::ISC_ENABLE)?;
    write_instruction(port, instruction::ISC_ERASE)?;
    write_instruction(port, instruction::ISC_NOOP)?;
    port.clock_for_ms(ERASE_CLOCK_MS)?;

    write_instruction(port, instruction::ERASE_DONE)?;
    write_instruction(port, instruction::ISC_NOOP)?;
    write_instruction(port, instruction::ISC_DISABLE)?;
    write_instruction(port, instruction::ISC_NOOP)?;
    port.clock_for_ms(ERASE_CLOCK_MS)?;

    write_instruction(port, instruction::ISC_ENABLE)?;
    write_instruction(port, instruction::FAST_PROGRAM)?;
    // Run-Test/Idle to Shift-DR.
    move_tap(port, 1, 2)?;
    shift_bitstream(port, rle)?;
    // Exit1-DR to Run-Test/Idle.
    move_tap(port, 1, 2)?;
    write_instruction(port, instruction::ISC_DISABLE)?;
    write_instruction(port, instruction::ISC_NOOP)?;
    Ok(())
}

/// Clocks `high` cycles with TMS high, then `low` with TMS low (M5GFX
/// `JTAG_MoveTap`).
fn move_tap<P: JtagPort>(port: &mut P, high: u8, low: u8) -> Result<(), P::Error> {
    if high > 0 {
        port.set_tms(true)?;
        port.clock(high)?;
    }
    port.set_tms(false)?;
    port.clock(low)
}

/// Shifts an eight-bit instruction through Shift-IR, least significant bit
/// first, and returns to Run-Test/Idle (M5GFX `JTAG_WriteInst`).
fn write_instruction<P: JtagPort>(port: &mut P, instruction: u8) -> Result<(), P::Error> {
    // Run-Test/Idle to Shift-IR.
    move_tap(port, 2, 2)?;
    let bits = instruction.reverse_bits();
    port.shift_bits(bits >> 1, 7)?;
    // The last bit leaves Shift-IR.
    port.set_tms(true)?;
    port.shift_bits(bits & 1, 1)?;
    // Exit1-IR to Update-IR, then Run-Test/Idle for ten more clocks.
    port.clock(1)?;
    port.set_tms(false)?;
    port.clock(11)
}

/// Shifts the decoded bitstream most significant bit first, raising TMS on
/// its last bit.
fn shift_bitstream<P: JtagPort>(port: &mut P, rle: &[u8]) -> Result<(), LoadError<P::Error>> {
    let mut runs = Runs::new(rle).peekable();
    let mut repeated = [0u8; 64];
    while let Some(run) = runs.next() {
        let run = run.map_err(|MalformedBitstream| LoadError::MalformedBitstream)?;
        let last_run = runs.peek().is_none();
        let (body, last): (&[u8], Option<u8>) = match run {
            Run::Repeat { byte, count } => {
                let mut remaining = usize::from(count);
                if last_run {
                    remaining = remaining.saturating_sub(1);
                }
                repeated.fill(byte);
                while remaining > 0 {
                    let chunk = remaining.min(repeated.len());
                    port.shift_bytes(repeated.get(..chunk).unwrap_or_default())?;
                    remaining = remaining.saturating_sub(chunk);
                }
                (&[], last_run.then_some(byte))
            }
            Run::Literal(bytes) => match bytes.split_last() {
                Some((&final_byte, body)) if last_run => (body, Some(final_byte)),
                _ => (bytes, None),
            },
        };
        port.shift_bytes(body)?;
        if let Some(byte) = last {
            port.shift_bits(byte >> 1, 7)?;
            port.set_tms(true)?;
            port.shift_bits(byte & 1, 1)?;
        }
    }
    Ok(())
}

/// Encodes the window a raw write fills, followed by `write` (M5GFX
/// `_set_window`): CASET with the first and last column, RASET with the
/// first and last row, all big-endian.
#[must_use]
pub fn window(x0: u16, y0: u16, x1: u16, y1: u16, write: u8) -> [u8; 11] {
    let [x0h, x0l] = x0.to_be_bytes();
    let [x1h, x1l] = x1.to_be_bytes();
    let [y0h, y0l] = y0.to_be_bytes();
    let [y1h, y1l] = y1.to_be_bytes();
    [
        command::CASET,
        x0h,
        x0l,
        x1h,
        x1l,
        command::RASET,
        y0h,
        y0l,
        y1h,
        y1l,
        write,
    ]
}

/// Encodes a rectangle filled with one RGB565 color (M5GFX `_fill_rect`):
/// first column and row, last column and row, and the color, big-endian.
#[must_use]
pub fn fill_rect_rgb565(x0: u16, y0: u16, x1: u16, y1: u16, color: u16) -> [u8; 11] {
    let [x0h, x0l] = x0.to_be_bytes();
    let [y0h, y0l] = y0.to_be_bytes();
    let [x1h, x1l] = x1.to_be_bytes();
    let [y1h, y1l] = y1.to_be_bytes();
    let [ch, cl] = color.to_be_bytes();
    [
        command::FILLRECT_16,
        x0h,
        x0l,
        y0h,
        y0l,
        x1h,
        x1l,
        y1h,
        y1l,
        ch,
        cl,
    ]
}

/// Encodes one RGB565 pixel (M5GFX `_fill_rect` of one pixel).
#[must_use]
pub fn draw_pixel_rgb565(x: u16, y: u16, color: u16) -> [u8; 7] {
    let [xh, xl] = x.to_be_bytes();
    let [yh, yl] = y.to_be_bytes();
    let [ch, cl] = color.to_be_bytes();
    [command::DRAWPIXEL_16, xh, xl, yh, yl, ch, cl]
}

/// One axis's sync, back porch, active, and front porch, in pixels or lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisTiming {
    /// Sync pulse.
    pub sync: u16,
    /// Back porch.
    pub back_porch: u16,
    /// Active pixels or lines.
    pub active: u16,
    /// Front porch.
    pub front_porch: u16,
}

impl AxisTiming {
    /// Returns the axis's whole period.
    #[must_use]
    pub fn total(self) -> u32 {
        [self.sync, self.back_porch, self.active, self.front_porch]
            .into_iter()
            .map(u32::from)
            .fold(0, u32::saturating_add)
    }
}

/// Encodes one axis's timing for `command` ([`command::VIDEO_TIMING_V`] or
/// [`command::VIDEO_TIMING_H`]) with M5GFX's checksum, the inverted byte sum.
#[must_use]
pub fn video_timing(command: u8, timing: AxisTiming) -> [u8; 10] {
    let mut frame = [0u8; 10];
    frame[0] = command;
    for (slot, value) in frame[1..9].as_chunks_mut::<2>().0.iter_mut().zip([
        timing.sync,
        timing.back_porch,
        timing.active,
        timing.front_porch,
    ]) {
        slot.copy_from_slice(&value.to_be_bytes());
    }
    frame[9] = checksum(&frame[..9]);
    frame
}

/// The FPGA PLL: `74.25 MHz × feedback / input` is the pixel clock;
/// `output` divides the VCO, which runs at the pixel clock times `output`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoClock {
    /// Input divider, 1 through 24.
    pub input_divider: u8,
    /// Feedback divider, 1 through 64.
    pub feedback_divider: u8,
    /// Output divider: 2, 4, 8, 16, 32, 48, 64, 80, 96, 112, or 128.
    pub output_divider: u8,
    /// Run the video logic at half the pixel clock, for pixel clocks above
    /// 74.25 MHz.
    pub half_clock: bool,
}

impl VideoClock {
    /// Chooses dividers for `target_hz` and returns them with the clock they
    /// make (M5GFX `getPllParams`).
    #[must_use]
    pub fn for_pixel_clock(target_hz: u32) -> (Self, u32) {
        let base = u64::from(BASE_CLOCK_HZ);
        let target = u64::from(target_hz);
        let mut feedback: u64 = 1;
        let mut input = base
            .checked_div(target.saturating_add(1))
            .unwrap_or(0)
            .max(1);
        let mut best = (2u64, 2u64);
        let mut best_diff = u64::MAX;
        loop {
            let clock = base
                .saturating_mul(feedback)
                .checked_div(input)
                .unwrap_or(0);
            let diff = target.abs_diff(clock);
            if best_diff > diff {
                best_diff = diff;
                best = (feedback, input);
                if diff == 0 {
                    break;
                }
            }
            if target < clock {
                input = input.saturating_add(1);
                if input > 24 {
                    break;
                }
            } else {
                feedback = feedback.saturating_add(1);
                if feedback > 64 {
                    break;
                }
            }
        }
        let (feedback, input) = best;
        let result = base
            .saturating_mul(feedback)
            .checked_div(input)
            .unwrap_or(0);
        let mut output = 8u8;
        let mut output_diff = u64::MAX;
        for divider in [2u8, 4, 8, 16, 32, 48, 64, 80, 96, 112, 128] {
            let diff = result
                .saturating_mul(u64::from(divider))
                .abs_diff(800_000_000);
            if output_diff < diff {
                break;
            }
            output_diff = diff;
            output = divider;
        }
        (
            Self {
                input_divider: u8::try_from(input).unwrap_or(u8::MAX),
                feedback_divider: u8::try_from(feedback).unwrap_or(u8::MAX),
                output_divider: output,
                half_clock: target_hz > BASE_CLOCK_HZ,
            },
            u32::try_from(result).unwrap_or(u32::MAX),
        )
    }

    /// Encodes the dividers with M5GFX's checksum (`_set_video_clock`).
    #[must_use]
    pub fn encode(self) -> [u8; 9] {
        let mut frame = [
            command::VIDEO_CLOCK,
            0,
            self.input_divider,
            0,
            self.feedback_divider,
            0,
            self.output_divider,
            u8::from(self.half_clock),
            0,
        ];
        frame[8] = checksum(&frame[..8]);
        frame
    }
}

/// Encodes the scaling of a `width` × `height` logical frame by `x_scale`
/// and `y_scale` (M5GFX `setScaling`, whose checksum is the same inverted
/// sum).
#[must_use]
pub fn scaling(x_scale: u8, y_scale: u8, width: u16, height: u16) -> [u8; 8] {
    let [wh, wl] = width.to_be_bytes();
    let [hh, hl] = height.to_be_bytes();
    let mut frame = [command::SCREEN_SCALING, x_scale, y_scale, wh, wl, hh, hl, 0];
    frame[7] = checksum(&frame[..7]);
    frame
}

fn checksum(bytes: &[u8]) -> u8 {
    !bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

/// The largest scale factor M5GFX allows.
const SCALE_MAX: u32 = 8;
/// The largest logical or output dimension M5GFX allows.
const RANGE_MAX: u32 = 2048;

/// A complete output mode: what the host draws, what the FPGA scans out,
/// and its timing (M5GFX `config_resolution` with automatic output size and
/// scale, then `_init_resolution`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoMode {
    /// The drawing size, which the FPGA scales up.
    pub logical: (u16, u16),
    /// The scanned-out size.
    pub output: (u16, u16),
    /// The scale factors.
    pub scale: (u8, u8),
    /// Where the logical frame starts in the scaled-down output frame.
    pub offset: (u16, u16),
    /// The refresh rate in hertz.
    pub refresh_hz: u32,
    /// The pixel-clock PLL.
    pub clock: VideoClock,
    /// Horizontal timing.
    pub horizontal: AxisTiming,
    /// Vertical timing.
    pub vertical: AxisTiming,
    /// Whether the blanking reached M5GFX's minimum; M5GFX still applies the
    /// mode when it does not, but warns that it may not display.
    pub within_limits: bool,
}

impl VideoMode {
    /// Derives the mode for a `width` × `height` drawing at `refresh_hz`
    /// (0 chooses 60, 30, or 24 Hz by size, as M5GFX does) on a
    /// `pixel_clock_hz` pixel clock. A zero size is 1280 × 720; one zero
    /// dimension follows 16:9.
    #[must_use]
    pub fn new(width: u16, height: u16, refresh_hz: u32, pixel_clock_hz: u32) -> Self {
        let (mut width, mut height) = (u32::from(width), u32::from(height));
        if width == 0 {
            if height == 0 {
                (width, height) = (1280, 720);
            } else {
                width = (height << 4).checked_div(9).unwrap_or(0);
            }
        } else if height == 0 {
            height = width.saturating_mul(9) >> 4;
        }
        let width = width.clamp(1, RANGE_MAX);
        let height = height.clamp(1, RANGE_MAX);
        let area = width.saturating_mul(height);
        let refresh = match refresh_hz {
            0 if area > 1_843_200 => 24,
            0 if area > 1_024_000 => 30,
            0 => 60,
            hz => hz.min(512),
        };
        let limit = 55_296_000u32.checked_div(refresh).unwrap_or(0);

        let mut scale_w = 1280u32.checked_div(width).unwrap_or(0);
        let mut scale_h = 720u32.checked_div(height).unwrap_or(0);
        if scale_w > SCALE_MAX
            || scale_h > SCALE_MAX
            || limit != 1280 * 720
            || scale_w.saturating_mul(width) != 1280
            || scale_h.saturating_mul(height) != 720
        {
            scale_w = 1;
            scale_h = 1;
            for scale in 2..=SCALE_MAX {
                let scaled_w = scale.saturating_mul(width);
                let scaled_h = scale.saturating_mul(height);
                if scaled_w > 1920 || scaled_h > 1920 || scaled_w.saturating_mul(scaled_h) > limit {
                    break;
                }
                scale_w = scale;
                scale_h = scale;
            }
        }
        let mut output_w = scale_w.saturating_mul(width);
        let mut output_h = scale_h.saturating_mul(height);
        if output_h & 1 == 1 {
            output_h = output_h.saturating_add(1);
        }
        if output_w & 1 == 1 && scale_w & 1 == 1 {
            output_w = output_w.saturating_add(scale_w);
        }
        if pixel_clock_hz > BASE_CLOCK_HZ && scale_w >= 2 {
            scale_w >>= 1;
            output_w >>= 1;
        }
        let offset_x = output_w
            .checked_div(scale_w)
            .unwrap_or(0)
            .saturating_sub(width)
            >> 1;
        let offset_y = output_h
            .checked_div(scale_h)
            .unwrap_or(0)
            .saturating_sub(height)
            >> 1;

        let (clock, output_clock) = VideoClock::for_pixel_clock(pixel_clock_hz);
        let half = u32::from(clock.half_clock);
        let (horizontal_total, vertical_total, within_limits) =
            blanking(output_w, output_h, scale_w, half, output_clock, refresh);

        let vertical = {
            let mut remain = vertical_total.saturating_sub(output_h).saturating_sub(1);
            let porch = remain >> 1;
            remain = remain.saturating_sub(porch);
            AxisTiming {
                sync: 1,
                back_porch: narrow(remain),
                active: narrow(output_h),
                front_porch: narrow(porch),
            }
        };
        let horizontal = {
            let mut remain = horizontal_total.saturating_sub(output_w);
            let sync = 24u32.saturating_add(remain >> 8);
            remain = remain.saturating_sub(sync);
            let porch = remain.saturating_mul(131) >> 8;
            remain = remain.saturating_sub(porch);
            AxisTiming {
                sync: narrow(sync),
                back_porch: narrow(remain),
                active: narrow(output_w),
                front_porch: narrow(porch),
            }
        };
        Self {
            logical: (narrow(width), narrow(height)),
            output: (narrow(output_w), narrow(output_h)),
            scale: (
                u8::try_from(scale_w).unwrap_or(1),
                u8::try_from(scale_h).unwrap_or(1),
            ),
            offset: (narrow(offset_x), narrow(offset_y)),
            refresh_hz: refresh,
            clock,
            horizontal,
            vertical,
            within_limits,
        }
    }

    /// Encodes the scaling command for this mode (M5GFX `setScaling`, which
    /// lowers the horizontal factor until it divides the output width).
    #[must_use]
    pub fn scaling(&self) -> [u8; 8] {
        let output_w = u32::from(self.output.0);
        let mut x_scale = u32::from(self.scale.0).max(1);
        let mut w = output_w.checked_div(x_scale).unwrap_or(0);
        while x_scale > 1 && w.saturating_mul(x_scale) != output_w {
            x_scale = x_scale.saturating_sub(1);
            w = output_w.checked_div(x_scale).unwrap_or(0);
        }
        let y_scale = u32::from(self.scale.1).max(1);
        let h = u32::from(self.output.1).checked_div(y_scale).unwrap_or(0);
        scaling(
            u8::try_from(x_scale).unwrap_or(1),
            u8::try_from(y_scale).unwrap_or(1),
            narrow(w),
            narrow(h),
        )
    }
}

/// Searches the horizontal and vertical totals whose product is closest to
/// the pixels one frame takes (M5GFX `_init_resolution`).
fn blanking(
    output_w: u32,
    output_h: u32,
    scale_w: u32,
    half: u32,
    output_clock: u32,
    refresh: u32,
) -> (u32, u32, bool) {
    let total = i64::from((output_clock >> half).checked_div(refresh).unwrap_or(0));
    let mut vertical_total = output_h.saturating_add(9);
    let mut horizontal_total = u32::try_from(total)
        .unwrap_or(0)
        .checked_div(vertical_total)
        .unwrap_or(0);
    let shift = 1u32.saturating_add(scale_w).min(31);
    let horizontal_min = output_w.saturating_add(32u32.saturating_add(output_w >> shift) >> half);
    let horizontal_max = output_w.saturating_add(768u32.saturating_add(output_w >> 3) >> half);
    let (mut h, mut v) = (horizontal_total.min(horizontal_max), vertical_total);
    let mut best = u64::MAX;
    // Every step raises the vertical total or lowers the horizontal one,
    // so the search ends long before this bound.
    for _ in 0..1_000_000 {
        let remainder = total.saturating_sub(i64::from(h).saturating_mul(i64::from(v)));
        let diff = remainder.unsigned_abs();
        if best > diff {
            best = diff;
            horizontal_total = h;
            vertical_total = v;
            if diff == 0 {
                break;
            }
        }
        if remainder >= 0 {
            v = v.saturating_add(1);
        } else {
            h = h.saturating_sub(1);
            if h < horizontal_min {
                break;
            }
        }
    }
    let within = horizontal_total > horizontal_min;
    if !within {
        horizontal_total = horizontal_min;
    }
    (horizontal_total, vertical_total, within)
}

fn narrow(value: u32) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::{
        AxisTiming, BITSTREAM_RLE, JtagPort, LoadError, MalformedBitstream, Run, Runs, VideoClock,
        VideoMode, command, decoded_len, draw_pixel_rgb565, fill_rect_rgb565, load_sram,
        video_timing, window,
    };

    /// One TCK cycle as the port saw it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Cycle {
        tms: bool,
        tdi: bool,
    }

    /// Records every clock, and models the TAP to report its states.
    #[derive(Default)]
    struct Recorder {
        tms: bool,
        cycles: Vec<Cycle>,
        clocked_ms: u32,
        waited_ms: u32,
    }

    impl JtagPort for Recorder {
        type Error = ();

        fn set_tms(&mut self, high: bool) -> Result<(), ()> {
            self.tms = high;
            Ok(())
        }

        fn clock(&mut self, cycles: u8) -> Result<(), ()> {
            for _ in 0..cycles {
                self.cycles.push(Cycle {
                    tms: self.tms,
                    tdi: false,
                });
            }
            Ok(())
        }

        fn shift_bits(&mut self, value: u8, bits: u8) -> Result<(), ()> {
            assert!((1..=8).contains(&bits));
            for bit in (0..bits).rev() {
                self.cycles.push(Cycle {
                    tms: self.tms,
                    tdi: value >> bit & 1 == 1,
                });
            }
            Ok(())
        }

        fn shift_bytes(&mut self, bytes: &[u8]) -> Result<(), ()> {
            for byte in bytes {
                self.shift_bits(*byte, 8)?;
            }
            Ok(())
        }

        fn clock_for_ms(&mut self, ms: u32) -> Result<(), ()> {
            self.clocked_ms += ms;
            self.clock(1)
        }

        fn delay_ms(&mut self, ms: u32) -> Result<(), ()> {
            self.waited_ms += ms;
            Ok(())
        }
    }

    /// The IEEE 1149.1 TAP state machine.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Tap {
        Reset,
        Idle,
        SelectDr,
        CaptureDr,
        ShiftDr,
        Exit1Dr,
        PauseDr,
        Exit2Dr,
        UpdateDr,
        SelectIr,
        CaptureIr,
        ShiftIr,
        Exit1Ir,
        PauseIr,
        Exit2Ir,
        UpdateIr,
    }

    impl Tap {
        fn next(self, tms: bool) -> Self {
            use Tap::*;
            match (self, tms) {
                (Reset, true) => Reset,
                (Reset, false) | (Idle, false) | (UpdateDr, false) | (UpdateIr, false) => Idle,
                (Idle, true) | (UpdateDr, true) | (UpdateIr, true) => SelectDr,
                (SelectDr, false) => CaptureDr,
                (SelectDr, true) => SelectIr,
                (CaptureDr, false) | (ShiftDr, false) | (Exit2Dr, false) => ShiftDr,
                (CaptureDr, true) | (ShiftDr, true) => Exit1Dr,
                (Exit1Dr, false) | (PauseDr, false) => PauseDr,
                (Exit1Dr, true) | (Exit2Dr, true) => UpdateDr,
                (PauseDr, true) => Exit2Dr,
                (SelectIr, false) => CaptureIr,
                (SelectIr, true) => Reset,
                (CaptureIr, false) | (ShiftIr, false) | (Exit2Ir, false) => ShiftIr,
                (CaptureIr, true) | (ShiftIr, true) => Exit1Ir,
                (Exit1Ir, false) | (PauseIr, false) => PauseIr,
                (Exit1Ir, true) | (Exit2Ir, true) => UpdateIr,
                (PauseIr, true) => Exit2Ir,
            }
        }
    }

    /// Replays the cycles through a TAP: the instructions updated, in order,
    /// and the bits shifted through Shift-DR.
    fn replay(cycles: &[Cycle]) -> (Vec<u8>, Vec<bool>, Tap) {
        let mut state = Tap::Reset;
        let mut instruction = 0u8;
        let mut instructions = Vec::new();
        let mut data = Vec::new();
        for cycle in cycles {
            match state {
                Tap::ShiftIr => instruction = instruction >> 1 | u8::from(cycle.tdi) << 7,
                Tap::ShiftDr => data.push(cycle.tdi),
                _ => {}
            }
            state = state.next(cycle.tms);
            if state == Tap::UpdateIr {
                instructions.push(instruction);
            }
        }
        (instructions, data, state)
    }

    #[test]
    fn the_bitstream_decodes_to_m5gfx_size() {
        assert_eq!(BITSTREAM_RLE.len(), 227_317);
        assert_eq!(decoded_len(BITSTREAM_RLE), Ok(444_430));
    }

    #[test]
    fn decodes_repeat_and_literal_records() {
        let rle = [3, 0xaa, 0, 2, 1, 2];
        let runs: Vec<_> = Runs::new(&rle).collect();
        assert_eq!(
            runs,
            [
                Ok(Run::Repeat {
                    byte: 0xaa,
                    count: 3
                }),
                Ok(Run::Literal(&[1, 2]))
            ]
        );
        assert_eq!(decoded_len(&[0, 3, 1]), Err(MalformedBitstream));
    }

    #[test]
    fn loads_in_m5gfx_order_and_leaves_the_tap_idle() {
        let rle = [2, 0x81, 0, 3, 0x12, 0x34, 0xc5];
        let mut port = Recorder::default();
        load_sram(&mut port, &rle).expect("load");
        let (instructions, data, state) = replay(&port.cycles);
        assert_eq!(
            instructions,
            [
                0x15, 0x05, 0x02, 0x09, 0x02, 0x3a, 0x02, 0x15, 0x17, 0x3a, 0x02
            ]
        );
        let bytes: Vec<u8> = data
            .chunks(8)
            .map(|bits| bits.iter().fold(0, |byte, bit| byte << 1 | u8::from(*bit)))
            .collect();
        // Exactly the decoded stream, most significant bit first.
        assert_eq!(bytes, [0x81, 0x81, 0x12, 0x34, 0xc5]);
        assert_eq!(state, Tap::Idle);
        assert_eq!(port.waited_ms, 128);
        assert_eq!(port.clocked_ms, 8);
    }

    #[test]
    fn a_final_repeat_raises_tms_on_its_last_bit_only() {
        let rle = [3, 0x0f];
        let mut port = Recorder::default();
        load_sram(&mut port, &rle).expect("load");
        let (_, data, _) = replay(&port.cycles);
        assert_eq!(data.len(), 24);
    }

    #[test]
    fn a_truncated_bitstream_fails() {
        let mut port = Recorder::default();
        assert_eq!(
            load_sram(&mut port, &[0, 4, 1]),
            Err(LoadError::MalformedBitstream)
        );
    }

    #[test]
    fn encodes_windows_rectangles_and_pixels_big_endian() {
        assert_eq!(
            window(0x0102, 0x0304, 0x0506, 0x0708, command::WRITE_RAW_16),
            [0x2a, 1, 2, 5, 6, 0x2b, 3, 4, 7, 8, 0x42]
        );
        assert_eq!(
            fill_rect_rgb565(1, 2, 3, 4, 0xf800),
            [0x6a, 0, 1, 0, 2, 0, 3, 0, 4, 0xf8, 0x00]
        );
        assert_eq!(
            draw_pixel_rgb565(5, 6, 0x07e0),
            [0x62, 0, 5, 0, 6, 0x07, 0xe0]
        );
    }

    #[test]
    fn checksums_invert_the_byte_sum() {
        let frame = video_timing(
            command::VIDEO_TIMING_V,
            AxisTiming {
                sync: 1,
                back_porch: 15,
                active: 720,
                front_porch: 14,
            },
        );
        assert_eq!(&frame[..9], &[0xb0, 0, 1, 0, 15, 2, 0xd0, 0, 14]);
        let sum = frame.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        assert_eq!(sum, 0xff);
    }

    #[test]
    fn the_default_mode_is_cea_720p() {
        let mode = VideoMode::new(1280, 720, 0, 74_250_000);
        assert_eq!(mode.output, (1280, 720));
        assert_eq!(mode.scale, (1, 1));
        assert_eq!(mode.refresh_hz, 60);
        assert_eq!(
            mode.clock,
            VideoClock {
                input_divider: 1,
                feedback_divider: 1,
                output_divider: 8,
                half_clock: false
            }
        );
        // The values M5GFX's arithmetic gives (a Python port of it agrees):
        // 1650 × 750 at 74.25 MHz, CEA-861 720p60's totals.
        assert_eq!(
            mode.horizontal,
            AxisTiming {
                sync: 25,
                back_porch: 169,
                active: 1280,
                front_porch: 176
            }
        );
        assert_eq!(
            mode.vertical,
            AxisTiming {
                sync: 1,
                back_porch: 15,
                active: 720,
                front_porch: 14
            }
        );
        assert_eq!(mode.horizontal.total() * mode.vertical.total(), 1_237_500);
        assert!(mode.within_limits);
        assert_eq!(
            mode.scaling(),
            [
                0x18,
                1,
                1,
                5,
                0,
                2,
                0xd0,
                0xff - 0x18 - 1 - 1 - 5 - 2 - 0xd0
            ]
        );
    }

    #[test]
    fn smaller_drawings_scale_up_to_720p() {
        let mode = VideoMode::new(640, 360, 0, 74_250_000);
        assert_eq!((mode.output, mode.scale), ((1280, 720), (2, 2)));
        let mode = VideoMode::new(320, 240, 0, 74_250_000);
        assert_eq!((mode.output, mode.scale), ((1280, 720), (4, 3)));
        let mode = VideoMode::new(960, 540, 0, 74_250_000);
        assert_eq!((mode.output, mode.scale), ((960, 540), (1, 1)));
        assert_eq!(mode.horizontal.front_porch, 339);
        assert_eq!(mode.vertical.back_porch, 105);
    }

    #[test]
    fn full_hd_falls_back_to_24_hertz() {
        let mode = VideoMode::new(1920, 1080, 0, 74_250_000);
        assert_eq!(mode.refresh_hz, 24);
        assert_eq!(mode.horizontal.total(), 1920 + 410 + 27 + 393);
    }
}

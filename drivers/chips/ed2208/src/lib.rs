//! Low-level Driver for the ED2208 E Ink Spectra 6 e-paper controller.
//!
//! M5Stack's M5GFX drives the PaperColor's 4-inch 400 x 600 Spectra 6 panel
//! through `Panel_ED2208`: an initialization list, one full frame of 4-bit
//! color indices (two pixels per byte, first pixel in the high nibble), and a
//! power-on, refresh, power-off sequence. `BUSY` is active low, and M5GFX
//! waits a further 200 ms after it rises. Spectra 6 has no partial update;
//! every refresh redraws the whole panel and takes 10 to 20 seconds.
//!
//! The panel shows six colors. [`dither_row`] converts RGB to them exactly as
//! M5GFX does: colors are chosen two pixels at a time, minimizing the error of
//! the pair's average plus each pixel's own error, after an ordered (Bayer)
//! or a phase-shifted per-channel offset.

#![no_std]

use embedded_hal::{
    delay::DelayNs,
    digital::{InputPin, OutputPin},
    spi::SpiDevice,
};

/// Panel color index of black.
pub const BLACK: u8 = 0x0;
/// Panel color index of white.
pub const WHITE: u8 = 0x1;
/// Panel color index of yellow.
pub const YELLOW: u8 = 0x2;
/// Panel color index of red.
pub const RED: u8 = 0x3;
/// Panel color index of blue.
pub const BLUE: u8 = 0x5;
/// Panel color index of green.
pub const GREEN: u8 = 0x6;

const POWER_OFF: u8 = 0x02;
const POWER_ON: u8 = 0x04;
const BOOSTER: u8 = 0x06;
const DEEP_SLEEP: u8 = 0x07;
const DATA_START: u8 = 0x10;
const DISPLAY_REFRESH: u8 = 0x12;
const RESOLUTION: u8 = 0x61;

/// Default `BUSY` timeout in milliseconds (M5GFX `_wait_busy`: 20 s).
pub const DEFAULT_BUSY_TIMEOUT_MS: u32 = 20_000;
/// Poll interval and post-busy settle time (M5GFX: 10 ms and 200 ms).
const BUSY_POLL_MS: u32 = 10;
const BUSY_SETTLE_MS: u32 = 200;
/// Fixed pauses of M5GFX `_turn_on_display`.
const POWER_SETTLE_MS: u32 = 200;

/// M5GFX `Panel_ED2208::getInitCommands` list 0, in order.
const INIT_SEQUENCE: &[(u8, &[u8])] = &[
    (0xAA, &[0x49, 0x55, 0x20, 0x08, 0x09, 0x18]),
    (0x01, &[0x3F]),
    (0x00, &[0x5F, 0x69]),
    (0x05, &[0x40, 0x1F, 0x1F, 0x2C]),
    (0x08, &[0x6F, 0x1F, 0x1F, 0x22]),
    (0x06, &[0x6F, 0x1F, 0x17, 0x17]),
    (0x03, &[0x03, 0x54, 0x00, 0x44]),
    (0x60, &[0x02, 0x00]),
    (0x30, &[0x08]),
    (0x50, &[0x3F]),
    (0xE3, &[0x2F]),
    (0x84, &[0x01]),
];

/// ED2208 transport or timing failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<SpiError, DcError, BusyError> {
    /// The SPI transfer failed.
    Spi(SpiError),
    /// The data/command output failed.
    DataCommand(DcError),
    /// The `BUSY` input could not be read.
    Busy(BusyError),
    /// `BUSY` stayed low past the timeout.
    Timeout,
}

/// Result type of [`Ed2208`] operations.
pub type Result<T, SPI, DC, BUSY> = core::result::Result<
    T,
    Error<
        <SPI as embedded_hal::spi::ErrorType>::Error,
        <DC as embedded_hal::digital::ErrorType>::Error,
        <BUSY as embedded_hal::digital::ErrorType>::Error,
    >,
>;

/// One ED2208 on a write-only four-wire SPI device.
pub struct Ed2208<SPI, DC, BUSY> {
    spi: SPI,
    dc: DC,
    busy: BUSY,
}

impl<SPI, DC, BUSY> Ed2208<SPI, DC, BUSY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    BUSY: InputPin,
{
    /// Takes the controller's SPI device, data/command output, and `BUSY`
    /// input. Nothing is sent.
    pub const fn new(spi: SPI, dc: DC, busy: BUSY) -> Self {
        Self { spi, dc, busy }
    }

    /// Returns the bus and pins.
    pub fn release(self) -> (SPI, DC, BUSY) {
        (self.spi, self.dc, self.busy)
    }

    /// Returns whether the controller is busy (`BUSY` low).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Busy`] when the input cannot be read.
    pub fn is_busy(&mut self) -> Result<bool, SPI, DC, BUSY> {
        self.busy.is_low().map_err(Error::Busy)
    }

    /// Polls `BUSY` every 10 ms until it rises, then waits 200 ms more, as
    /// M5GFX does. Returns at once when the controller is idle.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] after `timeout_ms`, or a pin error.
    pub fn wait_until_idle(
        &mut self,
        delay: &mut impl DelayNs,
        timeout_ms: u32,
    ) -> Result<(), SPI, DC, BUSY> {
        if !self.is_busy()? {
            return Ok(());
        }
        let mut waited = 0_u32;
        while self.is_busy()? {
            if waited >= timeout_ms {
                return Err(Error::Timeout);
            }
            delay.delay_ms(BUSY_POLL_MS);
            waited = waited.saturating_add(BUSY_POLL_MS);
        }
        delay.delay_ms(BUSY_SETTLE_MS);
        Ok(())
    }

    /// Sends one command and its data bytes.
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn command(&mut self, command: u8, data: &[u8]) -> Result<(), SPI, DC, BUSY> {
        self.dc.set_low().map_err(Error::DataCommand)?;
        self.spi.write(&[command]).map_err(Error::Spi)?;
        if !data.is_empty() {
            self.write_data(data)?;
        }
        Ok(())
    }

    /// Sends data bytes for the command in progress.
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn write_data(&mut self, data: &[u8]) -> Result<(), SPI, DC, BUSY> {
        self.dc.set_high().map_err(Error::DataCommand)?;
        self.spi.write(data).map_err(Error::Spi)
    }

    /// Runs M5GFX's initialization list and sets the resolution.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn initialize(
        &mut self,
        delay: &mut impl DelayNs,
        width: u16,
        height: u16,
    ) -> Result<(), SPI, DC, BUSY> {
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        for (command, data) in INIT_SEQUENCE {
            self.command(*command, data)?;
        }
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        let [width_high, width_low] = width.to_be_bytes();
        let [height_high, height_low] = height.to_be_bytes();
        self.command(
            RESOLUTION,
            &[width_high, width_low, height_high, height_low],
        )
    }

    /// Starts a frame transfer; follow with one [`Ed2208::write_data`] per
    /// row of packed color indices.
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn begin_frame(&mut self) -> Result<(), SPI, DC, BUSY> {
        self.command(DATA_START, &[])
    }

    /// Powers the panel on, refreshes it from the loaded frame, and powers it
    /// off again (M5GFX `_turn_on_display`).
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn refresh(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, DC, BUSY> {
        self.command(POWER_ON, &[])?;
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        delay.delay_ms(POWER_SETTLE_MS);
        self.command(BOOSTER, &[0x6F, 0x1F, 0x17, 0x27])?;
        delay.delay_ms(POWER_SETTLE_MS);
        self.command(DISPLAY_REFRESH, &[0x00])?;
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.command(POWER_OFF, &[0x00])?;
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        delay.delay_ms(POWER_SETTLE_MS);
        Ok(())
    }

    /// Enters deep sleep; a hardware reset wakes it.
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn deep_sleep(&mut self) -> Result<(), SPI, DC, BUSY> {
        self.command(DEEP_SLEEP, &[0xA5])
    }
}

/// One palette entry: the color index and M5GFX's ideal RGB for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PaletteColor {
    rgb: [i32; 3],
    index: u8,
}

/// M5GFX `epd_palette`.
const PALETTE: [PaletteColor; 6] = [
    PaletteColor {
        rgb: [0, 0, 0],
        index: BLACK,
    },
    PaletteColor {
        rgb: [255, 255, 255],
        index: WHITE,
    },
    PaletteColor {
        rgb: [255, 243, 56],
        index: YELLOW,
    },
    PaletteColor {
        rgb: [191, 0, 0],
        index: RED,
    },
    PaletteColor {
        rgb: [100, 64, 255],
        index: BLUE,
    },
    PaletteColor {
        rgb: [67, 138, 28],
        index: GREEN,
    },
];

/// M5GFX `bayer256`: a 16 x 16 ordered-dither matrix.
const BAYER_256: [u8; 256] = [
    0, 192, 48, 240, 12, 204, 60, 252, 3, 195, 51, 243, 15, 207, 63, 255, 128, 64, 176, 112, 140,
    76, 188, 124, 131, 67, 179, 115, 143, 79, 191, 127, 32, 224, 16, 208, 44, 236, 28, 220, 35,
    227, 19, 211, 47, 239, 31, 223, 160, 96, 144, 80, 172, 108, 156, 92, 163, 99, 147, 83, 175,
    111, 159, 95, 8, 200, 56, 248, 4, 196, 52, 244, 11, 203, 59, 251, 7, 199, 55, 247, 136, 72,
    184, 120, 132, 68, 180, 116, 139, 75, 187, 123, 135, 71, 183, 119, 40, 232, 24, 216, 36, 228,
    20, 212, 43, 235, 27, 219, 39, 231, 23, 215, 168, 104, 152, 88, 164, 100, 148, 84, 171, 107,
    155, 91, 167, 103, 151, 87, 2, 194, 50, 242, 14, 206, 62, 254, 1, 193, 49, 241, 13, 205, 61,
    253, 130, 66, 178, 114, 142, 78, 190, 126, 129, 65, 177, 113, 141, 77, 189, 125, 34, 226, 18,
    210, 46, 238, 30, 222, 33, 225, 17, 209, 45, 237, 29, 221, 162, 98, 146, 82, 174, 110, 158, 94,
    161, 97, 145, 81, 173, 109, 157, 93, 10, 202, 58, 250, 6, 198, 54, 246, 9, 201, 57, 249, 5,
    197, 53, 245, 138, 74, 186, 122, 134, 70, 182, 118, 137, 73, 185, 121, 133, 69, 181, 117, 42,
    234, 26, 218, 38, 230, 22, 214, 41, 233, 25, 217, 37, 229, 21, 213, 170, 106, 154, 90, 166,
    102, 150, 86, 169, 105, 153, 89, 165, 101, 149, 85,
];

/// Dithering applied when converting one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dither {
    /// Nearest color per pixel, no dither (M5GFX "fastest").
    None,
    /// One 16 x 16 Bayer offset on all channels, strength 70 (M5GFX "fast").
    Bayer,
    /// Phase-shifted per-channel offsets of the given strength (M5GFX "text"
    /// uses 70, "quality" 140).
    Rgb(u8),
}

/// Nearest palette index of one color (M5GFX `_rgb_to_epd_color`).
#[must_use]
pub fn nearest_color(rgb: [i32; 3]) -> u8 {
    let mut best = WHITE;
    let mut best_distance = u64::MAX;
    for color in PALETTE {
        let distance = squared_error(rgb, color.rgb);
        if distance < best_distance {
            best_distance = distance;
            best = color.index;
        }
    }
    best
}

fn squared_error(a: [i32; 3], b: [i32; 3]) -> u64 {
    a.iter()
        .zip(b)
        .map(|(a, b)| {
            let difference = i64::from(*a) - i64::from(b);
            u64::try_from(difference * difference).unwrap_or(u64::MAX)
        })
        .sum()
}

/// Packed pair of palette indices for two neighboring colors, minimizing the
/// error of their sum plus each one's own error (M5GFX
/// `_rgb_to_epd_color_pair`). The first color is the high nibble.
#[must_use]
pub fn nearest_pair(first: [i32; 3], second: [i32; 3]) -> u8 {
    let mut best = WHITE << 4 | WHITE;
    let mut best_distance = u64::MAX;
    for a in PALETTE {
        let error_a = [0, 1, 2].map(|channel| first[channel] - a.rgb[channel]);
        let own_a = squared_error(first, a.rgb);
        for b in PALETTE {
            let error_b = [0, 1, 2].map(|channel| second[channel] - b.rgb[channel]);
            let combined = [0, 1, 2].map(|channel| error_a[channel] + error_b[channel]);
            let distance = squared_error(combined, [0; 3])
                .saturating_add(own_a)
                .saturating_add(squared_error(second, b.rgb));
            if distance < best_distance {
                best_distance = distance;
                best = a.index << 4 | b.index;
            }
        }
    }
    best
}

/// Converts row `y` of 8-bit RGB pixels to packed color indices, two pixels
/// per byte, as M5GFX's `_dither_row_*` functions do. `out` must hold
/// `pixels.len().div_ceil(2)` bytes; extra bytes are left untouched.
pub fn dither_row(pixels: &[[u8; 3]], out: &mut [u8], y: usize, dither: Dither) {
    let widen = |pixel: [u8; 3]| pixel.map(i32::from);
    match dither {
        Dither::None => {
            for (pair, target) in pixels.chunks(2).zip(out.iter_mut()) {
                let first = nearest_color(widen(pair[0]));
                let second = pair
                    .get(1)
                    .map_or(WHITE, |pixel| nearest_color(widen(*pixel)));
                *target = first << 4 | second;
            }
        }
        Dither::Bayer => {
            let strength = 70;
            let row = (y & 15) << 4;
            let bias = |x: usize| {
                let value = i32::from(BAYER_256[row | (x & 15)]);
                ((value * 2 - 255) * strength) >> 8
            };
            for (index, target) in out.iter_mut().enumerate().take(pixels.len().div_ceil(2)) {
                let x = index * 2;
                let first = widen(pixels[x]).map(|channel| channel + bias(x));
                let second = pixels.get(x + 1).map_or([0; 3], |pixel| {
                    widen(*pixel).map(|channel| channel + bias(x + 1))
                });
                *target = nearest_pair(first, second);
            }
        }
        Dither::Rgb(strength) => dither_row_rgb(pixels, out, y, i32::from(strength)),
    }
}

/// M5GFX `_dither_row_rgb_pair`.
fn dither_row_rgb(pixels: &[[u8; 3]], out: &mut [u8], y: usize, strength: i32) {
    const X_STEP: i32 = 127 * 29;
    const Y_STEP: i32 = 129 * 48;
    const STEP_VALUE: i32 = 129 * 127;
    const STEP_DIFF: i32 = STEP_VALUE / 3;
    let wrap = |value: i32| if value < 0 { value + STEP_VALUE } else { value };
    let y = i32::try_from(y).unwrap_or(0);
    let mut bias_base = y.wrapping_mul(Y_STEP).rem_euclid(STEP_VALUE);
    let mut previous = [0_i32; 3];
    for x in 0..=pixels.len() {
        let mut color = pixels.get(x).map_or([128; 3], |pixel| pixel.map(i32::from));
        bias_base = wrap(bias_base - X_STEP);
        let bias_r = bias_base;
        let bias_g = wrap(bias_r - STEP_DIFF);
        let bias_b = wrap(bias_g - STEP_DIFF);
        let centered = [bias_r, bias_g, bias_b].map(|bias| bias * 2 - (STEP_VALUE - 1));
        let common = ((centered[0] + centered[1] + centered[2]) * strength) >> 16;
        for (channel, bias) in color.iter_mut().zip(centered) {
            *channel += common + ((bias * strength) >> 16);
        }
        if x & 1 == 1 {
            if let Some(target) = out.get_mut(x >> 1) {
                *target = nearest_pair(previous, color);
            }
        } else {
            previous = color;
        }
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
        digital::{ErrorType as PinErrorType, InputPin, OutputPin},
        spi::{ErrorType, Operation, SpiDevice},
    };

    use super::{
        BLACK, BLUE, Dither, Ed2208, Error, GREEN, RED, WHITE, YELLOW, dither_row, nearest_color,
        nearest_pair,
    };

    #[derive(Debug, PartialEq, Eq)]
    enum Sent {
        Command(u8),
        Data(Vec<u8>),
    }

    #[derive(Default)]
    struct Wire {
        dc_high: bool,
        sent: Vec<Sent>,
    }

    struct Spi<'a>(&'a RefCell<Wire>);
    struct Dc<'a>(&'a RefCell<Wire>);

    impl ErrorType for Spi<'_> {
        type Error = Infallible;
    }

    impl SpiDevice for Spi<'_> {
        fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Infallible> {
            let mut wire = self.0.borrow_mut();
            for operation in operations {
                if let Operation::Write(bytes) = operation {
                    let sent = if wire.dc_high {
                        Sent::Data(bytes.to_vec())
                    } else {
                        Sent::Command(bytes[0])
                    };
                    wire.sent.push(sent);
                }
            }
            Ok(())
        }
    }

    impl PinErrorType for Dc<'_> {
        type Error = Infallible;
    }

    impl OutputPin for Dc<'_> {
        fn set_low(&mut self) -> Result<(), Infallible> {
            self.0.borrow_mut().dc_high = false;
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Infallible> {
            self.0.borrow_mut().dc_high = true;
            Ok(())
        }
    }

    struct Busy {
        busy_reads: u32,
    }

    impl PinErrorType for Busy {
        type Error = Infallible;
    }

    impl InputPin for Busy {
        fn is_high(&mut self) -> Result<bool, Infallible> {
            Ok(!self.is_low()?)
        }

        fn is_low(&mut self) -> Result<bool, Infallible> {
            if self.busy_reads == 0 {
                Ok(false)
            } else {
                self.busy_reads -= 1;
                Ok(true)
            }
        }
    }

    #[derive(Default)]
    struct Delay {
        ms: u32,
    }

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.ms += ns / 1_000_000;
        }

        fn delay_ms(&mut self, ms: u32) {
            self.ms += ms;
        }
    }

    #[test]
    fn initialization_ends_with_the_resolution() {
        let wire = RefCell::new(Wire::default());
        let mut delay = Delay::default();
        Ed2208::new(Spi(&wire), Dc(&wire), Busy { busy_reads: 0 })
            .initialize(&mut delay, 400, 600)
            .expect("initialize");
        let sent = wire.into_inner().sent;
        assert_eq!(sent[0], Sent::Command(0xAA));
        assert_eq!(
            sent[1],
            Sent::Data(vec![0x49, 0x55, 0x20, 0x08, 0x09, 0x18])
        );
        assert_eq!(sent.len(), 26);
        assert_eq!(sent[24], Sent::Command(0x61));
        assert_eq!(sent[25], Sent::Data(vec![0x01, 0x90, 0x02, 0x58]));
        assert_eq!(delay.ms, 0);
    }

    #[test]
    fn refresh_powers_on_refreshes_and_powers_off() {
        let wire = RefCell::new(Wire::default());
        let mut delay = Delay::default();
        Ed2208::new(Spi(&wire), Dc(&wire), Busy { busy_reads: 3 })
            .refresh(&mut delay)
            .expect("refresh");
        assert_eq!(
            wire.into_inner().sent,
            vec![
                Sent::Command(0x04),
                Sent::Command(0x06),
                Sent::Data(vec![0x6F, 0x1F, 0x17, 0x27]),
                Sent::Command(0x12),
                Sent::Data(vec![0x00]),
                Sent::Command(0x02),
                Sent::Data(vec![0x00]),
            ]
        );
        // One busy period (two 10 ms polls, then 200 ms) and three 200 ms pauses.
        assert_eq!(delay.ms, 20 + 200 + 600);
    }

    #[test]
    fn stuck_busy_times_out() {
        let wire = RefCell::new(Wire::default());
        let mut delay = Delay::default();
        let mut panel = Ed2208::new(
            Spi(&wire),
            Dc(&wire),
            Busy {
                busy_reads: u32::MAX,
            },
        );
        assert_eq!(panel.wait_until_idle(&mut delay, 100), Err(Error::Timeout));
    }

    #[test]
    fn palette_colors_map_to_themselves() {
        assert_eq!(nearest_color([0, 0, 0]), BLACK);
        assert_eq!(nearest_color([255, 255, 255]), WHITE);
        assert_eq!(nearest_color([250, 240, 60]), YELLOW);
        assert_eq!(nearest_color([200, 10, 10]), RED);
        assert_eq!(nearest_color([90, 60, 250]), BLUE);
        assert_eq!(nearest_color([70, 140, 30]), GREEN);
        assert_eq!(nearest_pair([255; 3], [0; 3]), WHITE << 4 | BLACK);
    }

    #[test]
    fn every_dither_packs_two_pixels_per_byte() {
        let pixels = [[255, 255, 255]; 5];
        for dither in [
            Dither::None,
            Dither::Bayer,
            Dither::Rgb(70),
            Dither::Rgb(140),
        ] {
            let mut out = [0xEE_u8; 4];
            dither_row(&pixels, &mut out, 7, dither);
            assert_eq!(out[..2], [0x11, 0x11], "{dither:?}");
            assert_eq!(out[3], 0xEE, "{dither:?}");
            // The odd last pixel is white; Dither::None pads with white.
            assert_eq!(out[2] >> 4, WHITE, "{dither:?}");
        }
        let black = [[0, 0, 0]; 4];
        let mut out = [0_u8; 2];
        dither_row(&black, &mut out, 0, Dither::Rgb(140));
        assert_eq!(out, [0x00, 0x00]);
    }
}

//! Register-level driver for one GalaxyCore GC0308 VGA image sensor.
//!
//! The sensor answers SCCB (I2C) at 21h with 8-bit registers in two pages:
//! REGFEh bit 0 selects page 1, and writing F0h to it resets every register
//! to its default. Page 0 register 00h reads the chip ID 9Bh.
//!
//! The initialization table, the output-format field, and the subsampling
//! window follow Espressif's esp32-camera driver (`sensors/gc0308.c` and
//! `gc0308_settings.h`, Apache-2.0), which M5Stack's CoreS3 and
//! AtomS3R-CAM examples run. With that table the sensor drives VSYNC high
//! between frames and qualifies data with HREF high.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// The sensor's fixed seven-bit SCCB address.
pub const DEFAULT_ADDRESS: u8 = 0x21;
/// The chip ID in page 0 register 00h.
pub const CHIP_ID: u8 = 0x9b;
/// The width of the full sensor array the subsampling window is cut from.
pub const MAX_WIDTH: u16 = 640;
/// The height of the full sensor array the subsampling window is cut from.
pub const MAX_HEIGHT: u16 = 480;

/// REGFEh: soft reset (F0h) and page select (bit 0).
const RESET_RELATED: u8 = 0xfe;
const PAGE_0: u8 = 0x00;
const PAGE_1: u8 = 0x01;
const SOFT_RESET: u8 = 0xf0;
/// esp32-camera `reset`: 80 ms after the soft reset and after the table.
const RESET_SETTLE_MS: u32 = 80;
const CHIP_ID_REGISTER: u8 = 0x00;
/// Page 0 REG05h through REG0Ch: row start, column start, window height and
/// window width, each high byte then low byte.
const ROW_START_H: u8 = 0x05;
/// Page 0 REG24h: output format in bits 3:0.
const OUTPUT_FORMAT: u8 = 0x24;
/// Page 1 subsampling: REG53h bit 7 and REG55h bit 0 enable it, REG54h
/// selects the ratio, REG56h through REG59h its Y and UV offsets.
const SUBSAMPLE_ENABLE: u8 = 0x53;
const SUBSAMPLE_MODE: u8 = 0x54;
const SUBSAMPLE_ENABLE_2: u8 = 0x55;
const SUBSAMPLE_Y0: u8 = 0x56;
/// The window registers count 8 more rows and columns than the image.
const WINDOW_MARGIN: u16 = 8;

/// esp32-camera `gc0308_sensor_default_regs`, in order.
const DEFAULT_REGISTERS: &[(u8, u8)] = &[
    (0xfe, 0x00),
    (0xec, 0x20),
    (0x05, 0x00),
    (0x06, 0x00),
    (0x07, 0x00),
    (0x08, 0x00),
    (0x09, 0x01),
    (0x0a, 0xe8),
    (0x0b, 0x02),
    (0x0c, 0x88),
    (0x0d, 0x02),
    (0x0e, 0x02),
    (0x10, 0x26),
    (0x11, 0x0d),
    (0x12, 0x2a),
    (0x13, 0x00),
    (0x14, 0x10),
    (0x15, 0x0a),
    (0x16, 0x05),
    (0x17, 0x01),
    (0x18, 0x44),
    (0x19, 0x44),
    (0x1a, 0x2a),
    (0x1b, 0x00),
    (0x1c, 0x49),
    (0x1d, 0x9a),
    (0x1e, 0x61),
    (0x1f, 0x00),
    (0x20, 0x7f),
    (0x21, 0xfa),
    (0x22, 0x57),
    (0x24, 0xa2),
    (0x25, 0x0f),
    (0x26, 0x03),
    (0x28, 0x00),
    (0x2d, 0x0a),
    (0x2f, 0x01),
    (0x30, 0xf7),
    (0x31, 0x50),
    (0x32, 0x00),
    (0x33, 0x28),
    (0x34, 0x2a),
    (0x35, 0x28),
    (0x39, 0x04),
    (0x3a, 0x20),
    (0x3b, 0x20),
    (0x3c, 0x00),
    (0x3d, 0x00),
    (0x3e, 0x00),
    (0x3f, 0x00),
    (0x50, 0x14),
    (0x52, 0x41),
    (0x53, 0x80),
    (0x54, 0x80),
    (0x55, 0x80),
    (0x56, 0x80),
    (0x5a, 0x56),
    (0x5b, 0x40),
    (0x5c, 0x4a),
    (0x8b, 0x20),
    (0x8c, 0x20),
    (0x8d, 0x20),
    (0x8e, 0x14),
    (0x8f, 0x10),
    (0x90, 0x14),
    (0x91, 0x3c),
    (0x92, 0x50),
    (0x5d, 0x12),
    (0x5e, 0x1a),
    (0x5f, 0x24),
    (0x60, 0x07),
    (0x61, 0x15),
    (0x62, 0x08),
    (0x64, 0x03),
    (0x66, 0xe8),
    (0x67, 0x86),
    (0x68, 0x82),
    (0x69, 0x18),
    (0x6a, 0x0f),
    (0x6b, 0x00),
    (0x6c, 0x5f),
    (0x6d, 0x8f),
    (0x6e, 0x55),
    (0x6f, 0x38),
    (0x70, 0x15),
    (0x71, 0x33),
    (0x72, 0xdc),
    (0x73, 0x00),
    (0x74, 0x02),
    (0x75, 0x3f),
    (0x76, 0x02),
    (0x77, 0x38),
    (0x78, 0x88),
    (0x79, 0x81),
    (0x7a, 0x81),
    (0x7b, 0x22),
    (0x7c, 0xff),
    (0x93, 0x48),
    (0x94, 0x02),
    (0x95, 0x07),
    (0x96, 0xe0),
    (0x97, 0x40),
    (0x98, 0xf0),
    (0xb1, 0x40),
    (0xb2, 0x40),
    (0xb3, 0x40),
    (0xb6, 0xe0),
    (0xbd, 0x38),
    (0xbe, 0x36),
    (0xd0, 0xcb),
    (0xd1, 0x10),
    (0xd2, 0x90),
    (0xd3, 0x48),
    (0xd5, 0xf2),
    (0xd6, 0x16),
    (0xdb, 0x92),
    (0xdc, 0xa5),
    (0xdf, 0x23),
    (0xd9, 0x00),
    (0xda, 0x00),
    (0xe0, 0x09),
    (0xed, 0x04),
    (0xee, 0xa0),
    (0xef, 0x40),
    (0x80, 0x03),
    (0x9f, 0x10),
    (0xa0, 0x20),
    (0xa1, 0x38),
    (0xa2, 0x4e),
    (0xa3, 0x63),
    (0xa4, 0x76),
    (0xa5, 0x87),
    (0xa6, 0xa2),
    (0xa7, 0xb8),
    (0xa8, 0xca),
    (0xa9, 0xd8),
    (0xaa, 0xe3),
    (0xab, 0xeb),
    (0xac, 0xf0),
    (0xad, 0xf8),
    (0xae, 0xfd),
    (0xaf, 0xff),
    (0xc0, 0x00),
    (0xc1, 0x10),
    (0xc2, 0x1c),
    (0xc3, 0x30),
    (0xc4, 0x43),
    (0xc5, 0x54),
    (0xc6, 0x65),
    (0xc7, 0x75),
    (0xc8, 0x93),
    (0xc9, 0xb0),
    (0xca, 0xcb),
    (0xcb, 0xe6),
    (0xcc, 0xff),
    (0xf0, 0x02),
    (0xf1, 0x01),
    (0xf2, 0x02),
    (0xf3, 0x30),
    (0xf7, 0x04),
    (0xf8, 0x02),
    (0xf9, 0x9f),
    (0xfa, 0x78),
    (0xfe, 0x01),
    (0x00, 0xf5),
    (0x02, 0x20),
    (0x04, 0x10),
    (0x05, 0x08),
    (0x06, 0x20),
    (0x08, 0x0a),
    (0x0a, 0xa0),
    (0x0b, 0x60),
    (0x0c, 0x08),
    (0x0e, 0x44),
    (0x0f, 0x32),
    (0x10, 0x41),
    (0x11, 0x37),
    (0x12, 0x22),
    (0x13, 0x19),
    (0x14, 0x44),
    (0x15, 0x44),
    (0x16, 0xc2),
    (0x17, 0xa8),
    (0x18, 0x18),
    (0x19, 0x50),
    (0x1a, 0xd8),
    (0x1b, 0xf5),
    (0x70, 0x40),
    (0x71, 0x58),
    (0x72, 0x30),
    (0x73, 0x48),
    (0x74, 0x20),
    (0x75, 0x60),
    (0x77, 0x20),
    (0x78, 0x32),
    (0x30, 0x03),
    (0x31, 0x40),
    (0x32, 0x10),
    (0x33, 0xe0),
    (0x34, 0xe0),
    (0x35, 0x00),
    (0x36, 0x80),
    (0x37, 0x00),
    (0x38, 0x04),
    (0x39, 0x09),
    (0x3a, 0x12),
    (0x3b, 0x1c),
    (0x3c, 0x28),
    (0x3d, 0x31),
    (0x3e, 0x44),
    (0x3f, 0x57),
    (0x40, 0x6c),
    (0x41, 0x81),
    (0x42, 0x94),
    (0x43, 0xa7),
    (0x44, 0xb8),
    (0x45, 0xd6),
    (0x46, 0xee),
    (0x47, 0x0d),
    (0x62, 0xf7),
    (0x63, 0x68),
    (0x64, 0xd3),
    (0x65, 0xd3),
    (0x66, 0x60),
    (0xfe, 0x00),
    (0x01, 0x32),
    (0x02, 0x0c),
    (0x0f, 0x01),
    (0xe2, 0x00),
    (0xe3, 0x78),
    (0xe4, 0x00),
    (0xe5, 0xfe),
    (0xe6, 0x01),
    (0xe7, 0xe0),
    (0xe8, 0x01),
    (0xe9, 0xe0),
    (0xea, 0x01),
    (0xeb, 0xe0),
    (0xfe, 0x00),
];

/// One subsampling ratio over 420 and its page 1 registers 54h and 56h
/// through 59h (esp32-camera `subsample_cfgs`).
struct Subsample {
    numerator: u32,
    mode: u8,
    offsets: [u8; 4],
}

/// The denominator of every ratio in [`SUBSAMPLES`].
const SUBSAMPLE_DENOMINATOR: u32 = 420;

/// Coarsest ratio first, so the chosen window keeps the widest view.
const SUBSAMPLES: [Subsample; 8] = [
    Subsample {
        numerator: 84,
        mode: 0x55,
        offsets: [0x00, 0x00, 0x00, 0x00],
    },
    Subsample {
        numerator: 105,
        mode: 0x44,
        offsets: [0x00, 0x00, 0x00, 0x00],
    },
    Subsample {
        numerator: 140,
        mode: 0x33,
        offsets: [0x00, 0x00, 0x00, 0x00],
    },
    Subsample {
        numerator: 210,
        mode: 0x22,
        offsets: [0x00, 0x00, 0x00, 0x00],
    },
    Subsample {
        numerator: 240,
        mode: 0x77,
        offsets: [0x02, 0x46, 0x02, 0x46],
    },
    Subsample {
        numerator: 252,
        mode: 0x55,
        offsets: [0x02, 0x04, 0x02, 0x04],
    },
    Subsample {
        numerator: 280,
        mode: 0x33,
        offsets: [0x02, 0x00, 0x02, 0x00],
    },
    Subsample {
        numerator: 420,
        mode: 0x11,
        offsets: [0x00, 0x00, 0x00, 0x00],
    },
];

/// Pixel encoding on the eight data lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Two bytes per pixel, RGB565 in the order the sensor sends them
    /// (REG24h format 6).
    Rgb565,
    /// Two bytes per pixel, Y Cb Y Cr (REG24h format 2).
    Yuv422,
    /// One luma byte per pixel (REG24h B1h).
    Grayscale,
}

impl PixelFormat {
    /// Bytes the sensor sends per pixel.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u8 {
        match self {
            Self::Rgb565 | Self::Yuv422 => 2,
            Self::Grayscale => 1,
        }
    }
}

/// GC0308 access or configuration failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<BusError> {
    /// An SCCB register operation failed.
    Bus(BusError),
    /// Page 0 register 00h held another ID than 9Bh.
    UnexpectedChipId(u8),
    /// The requested image is empty or larger than 640x480.
    InvalidResolution,
}

/// One addressed GC0308; the bus is borrowed per call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gc0308 {
    address: u8,
}

impl Gc0308 {
    /// Addresses one sensor, or `None` for a non-seven-bit address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address > 0x7f {
            None
        } else {
            Some(Self { address })
        }
    }

    /// Selects page 0 and reads the chip ID.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the sensor does not answer.
    pub fn chip_id<I2C: I2c>(&self, i2c: &mut I2C) -> Result<u8, I2C::Error> {
        self.write(i2c, RESET_RELATED, PAGE_0)?;
        self.read(i2c, CHIP_ID_REGISTER)
    }

    /// Checks that the sensor answers with the GC0308 chip ID.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Bus`] when the sensor does not answer and
    /// [`Error::UnexpectedChipId`] for another part.
    pub fn probe<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), Error<I2C::Error>> {
        match self.chip_id(i2c).map_err(Error::Bus)? {
            CHIP_ID => Ok(()),
            other => Err(Error::UnexpectedChipId(other)),
        }
    }

    /// Resets the sensor, loads the default table, and selects the output
    /// format and a centred, subsampled window of `width` by `height`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidResolution`] before touching the sensor for an
    /// empty or oversized image, and [`Error::Bus`] for a failed transfer.
    pub fn initialize<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
        format: PixelFormat,
        width: u16,
        height: u16,
    ) -> Result<(), Error<I2C::Error>> {
        let window = Window::fit(width, height).ok_or(Error::InvalidResolution)?;
        self.write(i2c, RESET_RELATED, SOFT_RESET)
            .map_err(Error::Bus)?;
        delay.delay_ms(RESET_SETTLE_MS);
        for &(register, value) in DEFAULT_REGISTERS {
            self.write(i2c, register, value).map_err(Error::Bus)?;
        }
        delay.delay_ms(RESET_SETTLE_MS);
        self.set_pixel_format(i2c, format).map_err(Error::Bus)?;
        self.set_window(i2c, &window).map_err(Error::Bus)
    }

    fn set_pixel_format<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        format: PixelFormat,
    ) -> Result<(), I2C::Error> {
        self.write(i2c, RESET_RELATED, PAGE_0)?;
        match format {
            PixelFormat::Rgb565 => self.update(i2c, OUTPUT_FORMAT, 0x0f, 0x06),
            PixelFormat::Yuv422 => self.update(i2c, OUTPUT_FORMAT, 0x0f, 0x02),
            PixelFormat::Grayscale => self.write(i2c, OUTPUT_FORMAT, 0xb1),
        }
    }

    fn set_window<I2C: I2c>(&self, i2c: &mut I2C, window: &Window) -> Result<(), I2C::Error> {
        self.write(i2c, RESET_RELATED, PAGE_0)?;
        let [row_high, row_low] = window.row_start.to_be_bytes();
        let [column_high, column_low] = window.column_start.to_be_bytes();
        let [height_high, height_low] = window.height.to_be_bytes();
        let [width_high, width_low] = window.width.to_be_bytes();
        for (register, value) in (ROW_START_H..).zip([
            row_high,
            row_low,
            column_high,
            column_low,
            height_high,
            height_low,
            width_high,
            width_low,
        ]) {
            self.write(i2c, register, value)?;
        }
        self.write(i2c, RESET_RELATED, PAGE_1)?;
        self.update(i2c, SUBSAMPLE_ENABLE, 0x80, 0x80)?;
        self.update(i2c, SUBSAMPLE_ENABLE_2, 0x01, 0x01)?;
        self.write(i2c, SUBSAMPLE_MODE, window.subsample.mode)?;
        for (register, value) in (SUBSAMPLE_Y0..).zip(window.subsample.offsets) {
            self.write(i2c, register, value)?;
        }
        self.write(i2c, RESET_RELATED, PAGE_0)
    }

    /// Replaces the `mask` bits of one register with those of `value`.
    fn update<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        register: u8,
        mask: u8,
        value: u8,
    ) -> Result<(), I2C::Error> {
        let current = self.read(i2c, register)?;
        self.write(i2c, register, (current & !mask) | (value & mask))
    }

    fn read<I2C: I2c>(&self, i2c: &mut I2C, register: u8) -> Result<u8, I2C::Error> {
        let mut value = [0];
        i2c.write_read(self.address, &[register], &mut value)?;
        Ok(value[0])
    }

    fn write<I2C: I2c>(&self, i2c: &mut I2C, register: u8, value: u8) -> Result<(), I2C::Error> {
        i2c.write(self.address, &[register, value])
    }
}

/// The array window and subsampling ratio that yield one image size.
struct Window {
    row_start: u16,
    column_start: u16,
    /// Window height including the margin.
    height: u16,
    /// Window width including the margin.
    width: u16,
    subsample: &'static Subsample,
}

impl Window {
    /// esp32-camera `set_framesize` (subsample mode): the coarsest ratio
    /// whose subsampled array still covers the image, and the window that
    /// ratio scales down to it, centred in the array.
    fn fit(width: u16, height: u16) -> Option<Self> {
        if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
            return None;
        }
        let (image_width, image_height) = (u32::from(width), u32::from(height));
        let subsample = SUBSAMPLES.iter().find(|ratio| {
            scale(u32::from(MAX_WIDTH), ratio.numerator, SUBSAMPLE_DENOMINATOR) >= image_width
                && scale(
                    u32::from(MAX_HEIGHT),
                    ratio.numerator,
                    SUBSAMPLE_DENOMINATOR,
                ) >= image_height
        })?;
        let window_width = scale(image_width, SUBSAMPLE_DENOMINATOR, subsample.numerator);
        let window_height = scale(image_height, SUBSAMPLE_DENOMINATOR, subsample.numerator);
        let column_start = u32::from(MAX_WIDTH).checked_sub(window_width)? / 2;
        let row_start = u32::from(MAX_HEIGHT).checked_sub(window_height)? / 2;
        Some(Self {
            row_start: u16::try_from(row_start).ok()?,
            column_start: u16::try_from(column_start).ok()?,
            height: u16::try_from(window_height)
                .ok()?
                .checked_add(WINDOW_MARGIN)?,
            width: u16::try_from(window_width)
                .ok()?
                .checked_add(WINDOW_MARGIN)?,
            subsample,
        })
    }
}

/// `value * numerator / denominator` in integer arithmetic, as the vendor
/// code computes it; the operands here never overflow.
fn scale(value: u32, numerator: u32, denominator: u32) -> u32 {
    value
        .saturating_mul(numerator)
        .checked_div(denominator)
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{CHIP_ID, DEFAULT_ADDRESS, DEFAULT_REGISTERS, Error, Gc0308, PixelFormat};

    /// Two pages of 256 registers behind REGFEh, recording every write.
    struct Sensor {
        pages: [[u8; 256]; 2],
        page: usize,
        writes: Vec<(usize, u8, u8)>,
    }

    impl Default for Sensor {
        fn default() -> Self {
            let mut pages = [[0; 256]; 2];
            pages[0][0x00] = CHIP_ID;
            Self {
                pages,
                page: 0,
                writes: Vec::new(),
            }
        }
    }

    impl Sensor {
        fn written(&self, page: usize, register: u8) -> Vec<u8> {
            self.writes
                .iter()
                .filter(|(p, r, _)| *p == page && *r == register)
                .map(|(_, _, value)| *value)
                .collect()
        }
    }

    impl ErrorType for Sensor {
        type Error = Infallible;
    }

    impl I2c for Sensor {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, DEFAULT_ADDRESS);
            let mut register = None;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = Some(*reg),
                    Operation::Write([0xfe, value]) => {
                        self.writes.push((self.page, 0xfe, *value));
                        self.page = usize::from(*value & 0x01);
                    }
                    Operation::Write([reg, value]) => {
                        self.pages[self.page][usize::from(*reg)] = *value;
                        self.writes.push((self.page, *reg, *value));
                    }
                    Operation::Read([value]) => {
                        *value = self.pages[self.page][usize::from(register.expect("register"))];
                    }
                    _ => panic!("unexpected SCCB transfer"),
                }
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay {
        elapsed_ms: u32,
    }

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.elapsed_ms += ns / 1_000_000;
        }
    }

    fn sensor() -> Gc0308 {
        Gc0308::new(DEFAULT_ADDRESS).expect("seven-bit address")
    }

    #[test]
    fn rejects_an_eight_bit_address() {
        assert!(Gc0308::new(0x80).is_none());
    }

    #[test]
    fn probes_the_chip_id_on_page_0() {
        let mut bus = Sensor {
            page: 1,
            ..Sensor::default()
        };
        sensor().probe(&mut bus).expect("GC0308");
        assert_eq!(bus.writes, [(1, 0xfe, 0x00)]);

        bus.pages[0][0x00] = 0x26;
        assert_eq!(sensor().probe(&mut bus), Err(Error::UnexpectedChipId(0x26)));
    }

    #[test]
    fn initializes_qvga_rgb565_as_esp32_camera_does() {
        let mut bus = Sensor::default();
        let mut delay = Delay::default();
        sensor()
            .initialize(&mut bus, &mut delay, PixelFormat::Rgb565, 320, 240)
            .expect("initialize");

        // Soft reset first, then the vendor table in order.
        assert_eq!(bus.writes[0], (0, 0xfe, 0xf0));
        let table: Vec<(u8, u8)> = bus.writes[1..=DEFAULT_REGISTERS.len()]
            .iter()
            .map(|(_, register, value)| (*register, *value))
            .collect();
        assert_eq!(table, DEFAULT_REGISTERS);
        assert_eq!(delay.elapsed_ms, 160);

        // REG24h: the table's A2h with format 6 in the low nibble.
        assert_eq!(bus.pages[0][0x24], 0xa6);
        // 1/2 subsampling of the whole 640x480 array, plus the 8-line margin.
        assert_eq!(
            bus.pages[0][0x05..=0x0c],
            [0x00, 0x00, 0x00, 0x00, 0x01, 0xe8, 0x02, 0x88]
        );
        assert_eq!(bus.pages[1][0x53] & 0x80, 0x80);
        assert_eq!(bus.pages[1][0x55] & 0x01, 0x01);
        assert_eq!(bus.pages[1][0x54], 0x22);
        assert_eq!(bus.pages[1][0x56..=0x59], [0, 0, 0, 0]);
        // The sensor is left on page 0.
        assert_eq!(bus.page, 0);
    }

    #[test]
    fn centres_a_window_that_needs_a_fractional_ratio() {
        let mut bus = Sensor::default();
        sensor()
            .initialize(
                &mut bus,
                &mut Delay::default(),
                PixelFormat::Yuv422,
                400,
                296,
            )
            .expect("initialize");
        // 400x296 needs the 2/3 ratio: a 600x444 window at column 20, row 18.
        assert_eq!(
            bus.pages[0][0x05..=0x0c],
            [0x00, 0x12, 0x00, 0x14, 0x01, 0xc4, 0x02, 0x60]
        );
        assert_eq!(bus.pages[1][0x54], 0x33);
        assert_eq!(bus.pages[1][0x56..=0x59], [0x02, 0x00, 0x02, 0x00]);
        assert_eq!(bus.pages[0][0x24] & 0x0f, 0x02);
    }

    #[test]
    fn selects_grayscale_by_writing_the_whole_format_register() {
        let mut bus = Sensor::default();
        sensor()
            .initialize(
                &mut bus,
                &mut Delay::default(),
                PixelFormat::Grayscale,
                160,
                120,
            )
            .expect("initialize");
        assert_eq!(bus.written(0, 0x24).last(), Some(&0xb1));
        assert_eq!(bus.pages[1][0x54], 0x44);
    }

    #[test]
    fn refuses_an_unsupported_image_before_any_transfer() {
        let mut bus = Sensor::default();
        assert_eq!(
            sensor().initialize(
                &mut bus,
                &mut Delay::default(),
                PixelFormat::Rgb565,
                800,
                600
            ),
            Err(Error::InvalidResolution)
        );
        assert_eq!(
            sensor().initialize(&mut bus, &mut Delay::default(), PixelFormat::Rgb565, 0, 240),
            Err(Error::InvalidResolution)
        );
        assert!(bus.writes.is_empty());
    }
}

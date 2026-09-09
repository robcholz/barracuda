//! Register-level Goodix GT911 touch-controller driver.

#![no_std]

use embedded_hal::i2c::I2c;

const PRODUCT_ID: u16 = 0x8140;
const STATUS: u16 = 0x814e;
const FIRST_POINT: u16 = 0x8150;
const REPORT_BYTES: usize = 8;
const DATA_READY: u8 = 1 << 7;

/// Maximum contacts reported by the GT911 data block.
pub const MAX_POINTS: usize = 5;

/// One chip-native contact report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    /// Controller-assigned track identifier.
    pub id: u8,
    /// Horizontal coordinate.
    pub x: u16,
    /// Vertical coordinate.
    pub y: u16,
    /// Contact area reported by the controller.
    pub size: u16,
}

/// One fixed-capacity GT911 report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    points: [Point; MAX_POINTS],
    len: u8,
}

impl Report {
    /// Returns the valid contacts in this report.
    #[must_use]
    pub fn points(&self) -> &[Point] {
        &self.points[..usize::from(self.len)]
    }
}

/// GT911 transport or report failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// The controller advertised more contacts than its report block can hold.
    InvalidReport,
}

/// A GT911 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gt911 {
    address: u8,
}

impl Gt911 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Returns whether the product-ID register contains an ASCII product code.
    pub fn is_present<I2C: I2c>(&self, i2c: &mut I2C) -> Result<bool, I2C::Error> {
        let mut product = [0; 4];
        self.read(i2c, PRODUCT_ID, &mut product)?;
        Ok(product[..3].iter().all(u8::is_ascii_digit))
    }

    /// Reads and acknowledges the latest contact report.
    pub fn read_report<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Report, Error<I2C::Error>> {
        let mut status = [0];
        self.read(i2c, STATUS, &mut status).map_err(Error::Bus)?;
        if status[0] & DATA_READY == 0 {
            return Ok(Report {
                points: [Point::default(); MAX_POINTS],
                len: 0,
            });
        }
        let count = status[0] & 0x0f;
        if usize::from(count) > MAX_POINTS {
            return Err(Error::InvalidReport);
        }
        let mut data = [0; MAX_POINTS * REPORT_BYTES];
        let length = usize::from(count).saturating_mul(REPORT_BYTES);
        let read_result = self.read(i2c, FIRST_POINT, &mut data[..length]);
        let clear_result = self.write_byte(i2c, STATUS, 0);
        read_result.map_err(Error::Bus)?;
        clear_result.map_err(Error::Bus)?;

        let mut points = [Point::default(); MAX_POINTS];
        let (reports, _) = data[..length].as_chunks::<REPORT_BYTES>();
        for (target, report) in points.iter_mut().zip(reports) {
            *target = Point {
                id: report[0],
                x: u16::from_le_bytes([report[1], report[2]]),
                y: u16::from_le_bytes([report[3], report[4]]),
                size: u16::from_le_bytes([report[5], report[6]]),
            };
        }
        Ok(Report { points, len: count })
    }

    fn read<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        register: u16,
        data: &mut [u8],
    ) -> Result<(), I2C::Error> {
        i2c.write_read(self.address, &register.to_be_bytes(), data)
    }

    fn write_byte<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        register: u16,
        value: u8,
    ) -> Result<(), I2C::Error> {
        let [high, low] = register.to_be_bytes();
        i2c.write(self.address, &[high, low, value])
    }
}

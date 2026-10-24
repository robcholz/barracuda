//! Report-level driver for the Parade TT21100 touch controller.
//!
//! The protocol follows Espressif's `esp_lcd_touch_tt21100` component
//! (esp-bsp `components/lcd_touch/esp_lcd_touch_tt21100`). The controller
//! has no register address: a plain two-byte read returns the length of the
//! pending report, little-endian, and a read of that length returns the
//! report itself, length first. A 14-byte report carries the capacitive
//! buttons; a touch report has a 7-byte header, whose sixth byte holds the
//! record count in bits 4:0, followed by one 10-byte record per contact:
//! touch type (bits 2:0), track ID (bits 4:0 of the second byte), X and Y
//! as little-endian words, and the pressure.

#![no_std]

use embedded_hal::i2c::I2c;

/// The controller's fixed seven-bit address (`ESP_LCD_TOUCH_IO_I2C_TT21100_ADDRESS`).
pub const ADDRESS: u8 = 0x24;

/// Maximum contacts this driver decodes from one report.
pub const MAX_POINTS: usize = 10;

const HEADER_BYTES: usize = 7;
const RECORD_BYTES: usize = 10;
const BUTTON_REPORT_BYTES: usize = 14;
const REPORT_CAPACITY: usize = HEADER_BYTES + MAX_POINTS * RECORD_BYTES;
/// Touch type 0 is a finger; others (stylus, glove, hover) are skipped, as
/// the Espressif component does.
const FINGER: u8 = 0;

/// One chip-native contact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    /// Controller-assigned track identifier.
    pub id: u8,
    /// Horizontal coordinate.
    pub x: u16,
    /// Vertical coordinate.
    pub y: u16,
    /// Pressure.
    pub pressure: u8,
}

/// The finger contacts of one report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    points: [Point; MAX_POINTS],
    len: u8,
}

impl Report {
    const fn empty() -> Self {
        Self {
            points: [Point {
                id: 0,
                x: 0,
                y: 0,
                pressure: 0,
            }; MAX_POINTS],
            len: 0,
        }
    }

    /// Returns the valid contacts in this report.
    #[must_use]
    pub fn points(&self) -> &[Point] {
        &self.points[..usize::from(self.len)]
    }
}

/// A TT21100 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tt21100 {
    address: u8,
}

impl Tt21100 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Reports whether the controller acknowledges a length read; a bus
    /// failure means no controller.
    pub fn is_present<I2C: I2c>(&self, i2c: &mut I2C) -> bool {
        let mut length = [0; 2];
        i2c.read(self.address, &mut length).is_ok()
    }

    /// Reads the pending report and returns its finger contacts. A button
    /// report, an empty queue, or a length beyond the decoded capacity
    /// returns no contacts.
    ///
    /// # Errors
    ///
    /// Returns the bus error.
    pub fn read_report<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Report, I2C::Error> {
        let mut length = [0; 2];
        i2c.read(self.address, &mut length)?;
        let length = usize::from(u16::from_le_bytes(length));
        if length == 0 || length >= REPORT_CAPACITY {
            return Ok(Report::empty());
        }
        let mut data = [0; REPORT_CAPACITY];
        let data = &mut data[..length];
        i2c.read(self.address, data)?;
        if length == BUTTON_REPORT_BYTES || length < HEADER_BYTES {
            return Ok(Report::empty());
        }
        let records = usize::from(data[5] & 0x1f);
        let mut report = Report::empty();
        let (_, body) = data.split_at(HEADER_BYTES);
        let (records_data, _) = body.as_chunks::<RECORD_BYTES>();
        for record in records_data.iter().take(records.min(MAX_POINTS)) {
            if record[0] & 0x07 != FINGER {
                continue;
            }
            let Some(slot) = report.points.get_mut(usize::from(report.len)) else {
                break;
            };
            *slot = Point {
                id: record[1] & 0x1f,
                x: u16::from_le_bytes([record[2], record[3]]),
                y: u16::from_le_bytes([record[4], record[5]]),
                pressure: record[6],
            };
            report.len = report.len.saturating_add(1);
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{ADDRESS, Point, Tt21100};

    /// Returns `report` from the start on every read, as the controller
    /// does.
    struct Controller {
        report: Vec<u8>,
    }

    impl ErrorType for Controller {
        type Error = Infallible;
    }

    impl I2c for Controller {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, ADDRESS);
            for operation in operations {
                match operation {
                    Operation::Read(bytes) => {
                        for (target, source) in bytes
                            .iter_mut()
                            .zip(self.report.iter().chain(core::iter::repeat(&0)))
                        {
                            *target = *source;
                        }
                    }
                    Operation::Write(_) => panic!("reports are plain reads"),
                }
            }
            Ok(())
        }
    }

    fn chip() -> Tt21100 {
        Tt21100::new(ADDRESS).expect("valid address")
    }

    #[test]
    fn decodes_finger_records_and_skips_other_touch_types() {
        let mut report = Vec::from([27, 0, 0x01, 0x34, 0x12, 0x02, 0x00]);
        // Finger, track 3, (300, 200), pressure 40.
        report.extend([0x00, 0x83, 0x2c, 0x01, 0xc8, 0x00, 40, 0, 0, 0]);
        // A hovering contact (type 1) is not a touch.
        report.extend([0x01, 0x04, 0x10, 0x00, 0x10, 0x00, 5, 0, 0, 0]);
        let mut bus = Controller { report };
        let report = chip().read_report(&mut bus).expect("report");
        assert_eq!(
            report.points(),
            [Point {
                id: 3,
                x: 300,
                y: 200,
                pressure: 40,
            }]
        );
        assert!(chip().is_present(&mut bus));
    }

    #[test]
    fn button_and_empty_reports_carry_no_contacts() {
        let mut bus = Controller {
            report: Vec::from([14, 0, 0x03, 0, 0, 0x01, 0, 0, 0, 0, 0, 0, 0, 0]),
        };
        assert!(
            chip()
                .read_report(&mut bus)
                .expect("report")
                .points()
                .is_empty()
        );
        let mut bus = Controller {
            report: Vec::from([0, 0]),
        };
        assert!(
            chip()
                .read_report(&mut bus)
                .expect("report")
                .points()
                .is_empty()
        );
        let mut bus = Controller {
            report: Vec::from([0xff, 0x00]),
        };
        assert!(
            chip()
                .read_report(&mut bus)
                .expect("report")
                .points()
                .is_empty()
        );
    }
}

//! Register-level Chipsemi CHSC6540 touch-controller driver.
//!
//! The sequence and report layout follow M5GFX's `Touch_CHSC6540` (used on
//! the M5Stack Tough): start-up writes 5Ah 5Ah to switch the interrupt mode,
//! and a report is the contact count at 02h followed by six bytes per
//! contact from 03h. The controller reports at most two contacts and no
//! track identifiers, so contacts are numbered in report order.

#![no_std]

use embedded_hal::i2c::I2c;

const INTERRUPT_MODE_CHANGE: [u8; 2] = [0x5a, 0x5a];
const CONTACT_COUNT: u8 = 0x02;
const POINT_BYTES: usize = 6;

/// The address M5GFX uses on the M5Stack Tough.
pub const DEFAULT_ADDRESS: u8 = 0x2e;

/// Most contacts the controller reports.
pub const MAX_POINTS: usize = 2;

/// One chip-native contact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    /// Position of the contact in the report.
    pub id: u8,
    /// Horizontal coordinate.
    pub x: u16,
    /// Vertical coordinate.
    pub y: u16,
}

/// One fixed-capacity report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    points: [Point; MAX_POINTS],
    len: u8,
}

impl Report {
    /// Returns the contacts in this report.
    #[must_use]
    pub fn points(&self) -> &[Point] {
        &self.points[..usize::from(self.len)]
    }
}

/// A CHSC6540 at a caller-selected I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chsc6540 {
    address: u8,
}

impl Chsc6540 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Switches the interrupt mode as M5GFX does at start-up; an absent
    /// controller does not acknowledge it.
    ///
    /// # Errors
    ///
    /// Returns the bus error of the write.
    pub fn initialize<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), I2C::Error> {
        i2c.write(self.address, &INTERRUPT_MODE_CHANGE)
    }

    /// Reads the current contacts.
    ///
    /// # Errors
    ///
    /// Returns the bus error of a failed read.
    pub fn read_report<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Report, I2C::Error> {
        let mut count = [0];
        i2c.write_read(self.address, &[CONTACT_COUNT], &mut count)?;
        let count = usize::from(count[0]).min(MAX_POINTS);
        let mut report = Report {
            points: [Point::default(); MAX_POINTS],
            len: 0,
        };
        if count == 0 {
            return Ok(report);
        }
        let mut data = [0; MAX_POINTS * POINT_BYTES];
        let data = &mut data[..count.saturating_mul(POINT_BYTES)];
        i2c.write_read(self.address, &[CONTACT_COUNT.saturating_add(1)], data)?;
        for (id, (point, bytes)) in report
            .points
            .iter_mut()
            .zip(data.as_chunks::<POINT_BYTES>().0)
            .enumerate()
        {
            *point = Point {
                id: id as u8,
                x: u16::from(bytes[0] & 0x0f) << 8 | u16::from(bytes[1]),
                y: u16::from(bytes[2] & 0x0f) << 8 | u16::from(bytes[3]),
            };
        }
        report.len = count as u8;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::Chsc6540;

    struct Registers {
        memory: [u8; 256],
        writes: Vec<Vec<u8>>,
    }

    impl ErrorType for Registers {
        type Error = Infallible;
    }

    impl I2c for Registers {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x2e);
            let mut pointer = 0_usize;
            let reads = operations
                .iter()
                .any(|operation| matches!(operation, Operation::Read(_)));
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        pointer = usize::from(bytes[0]);
                        if !reads {
                            self.writes.push(bytes.to_vec());
                        }
                    }
                    Operation::Read(buffer) => {
                        for byte in buffer.iter_mut() {
                            *byte = self.memory[pointer];
                            pointer += 1;
                        }
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn starts_with_the_interrupt_mode_change() {
        let mut bus = Registers {
            memory: [0; 256],
            writes: Vec::new(),
        };
        let chip = Chsc6540::new(0x2e).expect("valid address");
        chip.initialize(&mut bus).expect("start-up");
        assert_eq!(bus.writes, [std::vec![0x5a, 0x5a]]);
    }

    #[test]
    fn decodes_up_to_two_contacts_in_report_order() {
        let mut bus = Registers {
            memory: [0; 256],
            writes: Vec::new(),
        };
        bus.memory[0x02] = 3;
        bus.memory[0x03..0x09].copy_from_slice(&[0x81, 0x3f, 0x00, 0xef, 0, 0]);
        bus.memory[0x09..0x0f].copy_from_slice(&[0x00, 0x20, 0x01, 0x17, 0, 0]);
        let chip = Chsc6540::new(0x2e).expect("valid address");
        let report = chip.read_report(&mut bus).expect("report");
        let points = report.points();
        assert_eq!(points.len(), 2);
        assert_eq!((points[0].id, points[0].x, points[0].y), (0, 0x13f, 0x0ef));
        assert_eq!((points[1].id, points[1].x, points[1].y), (1, 0x020, 0x117));

        bus.memory[0x02] = 0;
        assert!(
            chip.read_report(&mut bus)
                .expect("report")
                .points()
                .is_empty()
        );
    }
}

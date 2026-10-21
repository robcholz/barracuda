//! Command-level Hynitron CST3530 touch-controller driver.
//!
//! The CST3530 takes 32-bit command words instead of register addresses.
//! The sequence and report layout follow M5GFX's `Touch_CST3530` (used on
//! the M5Stack CoreP4X): start-up writes D0000400h twice 20 ms apart, then
//! D0000000h, D0000C00h, and D0000100h. A report is read from D0070000h:
//! nine bytes, five more per further key or contact, then D00002ABh
//! acknowledges it. Byte 3 holds the key count (high nibble) and contact
//! count (low nibble); each five-byte entry, keys first, holds the low eight
//! bits of x and y at offsets 4 and 5 and their high nibbles at offset 7.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

const START_UP: [u32; 5] = [
    0xd000_0400,
    0xd000_0400,
    0xd000_0000,
    0xd000_0c00,
    0xd000_0100,
];
const START_UP_SETTLE_MS: u32 = 20;
const REPORT: u32 = 0xd007_0000;
const ACKNOWLEDGE: u32 = 0xd000_02ab;
const HEADER_BYTES: usize = 9;
const ENTRY_BYTES: usize = 5;
const REPORT_BYTES: usize = 50;

/// The address M5GFX uses.
pub const DEFAULT_ADDRESS: u8 = 0x58;

/// Most contacts M5GFX takes from one report.
pub const MAX_POINTS: usize = 5;

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
    const EMPTY: Self = Self {
        points: [Point { id: 0, x: 0, y: 0 }; MAX_POINTS],
        len: 0,
    };

    /// Returns the contacts in this report.
    #[must_use]
    pub fn points(&self) -> &[Point] {
        &self.points[..usize::from(self.len)]
    }

    /// Decodes a report buffer read from D0070000h.
    #[must_use]
    pub fn decode(data: &[u8; REPORT_BYTES]) -> Self {
        let contacts = usize::from(data[3] & 0x0f);
        let keys = usize::from(data[3] >> 4);
        let mut report = Self::EMPTY;
        // M5GFX treats a report whose byte 8 has a zero high nibble as empty.
        if contacts == 0 || data[8] >> 4 == 0 {
            return report;
        }
        let count = contacts.min(MAX_POINTS);
        for (index, point) in report.points.iter_mut().take(count).enumerate() {
            let base = keys.saturating_add(index).saturating_mul(ENTRY_BYTES);
            let (Some(&x_low), Some(&y_low), Some(&high)) = (
                data.get(base.saturating_add(4)),
                data.get(base.saturating_add(5)),
                data.get(base.saturating_add(7)),
            ) else {
                break;
            };
            *point = Point {
                id: index as u8,
                x: u16::from(x_low) | u16::from(high & 0x0f) << 8,
                y: u16::from(y_low) | u16::from(high & 0xf0) << 4,
            };
            report.len = report.len.saturating_add(1);
        }
        report
    }
}

/// A CST3530 at a caller-selected I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cst3530 {
    address: u8,
}

impl Cst3530 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Sends M5GFX's start-up command words.
    ///
    /// # Errors
    ///
    /// Returns the bus error of the first command the controller does not
    /// acknowledge.
    pub fn initialize<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<(), I2C::Error> {
        for (index, command) in START_UP.into_iter().enumerate() {
            i2c.write(self.address, &command.to_be_bytes())?;
            if index < 2 {
                delay.delay_ms(START_UP_SETTLE_MS);
            }
        }
        Ok(())
    }

    /// Reads and acknowledges the current report.
    ///
    /// # Errors
    ///
    /// Returns the bus error of a failed read or acknowledgement.
    pub fn read_report<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Report, I2C::Error> {
        let mut data = [0; REPORT_BYTES];
        i2c.write_read(
            self.address,
            &REPORT.to_be_bytes(),
            &mut data[..HEADER_BYTES],
        )?;
        let entries = usize::from(data[3] >> 4).saturating_add(usize::from(data[3] & 0x0f));
        let extra = entries
            .saturating_sub(1)
            .saturating_mul(ENTRY_BYTES)
            .min(REPORT_BYTES - HEADER_BYTES);
        let read = if extra > 0 {
            i2c.read(self.address, &mut data[HEADER_BYTES..][..extra])
        } else {
            Ok(())
        };
        // M5GFX acknowledges the report whether or not the rest was read.
        let acknowledged = i2c.write(self.address, &ACKNOWLEDGE.to_be_bytes());
        read?;
        acknowledged?;
        Ok(Report::decode(&data))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::Cst3530;

    #[derive(Default)]
    struct Bus {
        writes: Vec<Vec<u8>>,
        reads: Vec<Vec<u8>>,
    }

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x58);
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => self.writes.push(bytes.to_vec()),
                    Operation::Read(buffer) => {
                        let data = self.reads.remove(0);
                        buffer.copy_from_slice(&data);
                    }
                }
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay(u32);

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.0 += ns / 1_000_000;
        }
    }

    #[test]
    fn starts_with_m5gfx_command_words() {
        let mut bus = Bus::default();
        let mut delay = Delay::default();
        Cst3530::new(0x58)
            .expect("valid address")
            .initialize(&mut bus, &mut delay)
            .expect("start-up");
        assert_eq!(
            bus.writes,
            [
                std::vec![0xd0, 0x00, 0x04, 0x00],
                std::vec![0xd0, 0x00, 0x04, 0x00],
                std::vec![0xd0, 0x00, 0x00, 0x00],
                std::vec![0xd0, 0x00, 0x0c, 0x00],
                std::vec![0xd0, 0x00, 0x01, 0x00],
            ]
        );
        assert_eq!(delay.0, 40);
    }

    #[test]
    fn reads_two_contacts_and_acknowledges_the_report() {
        let mut bus = Bus::default();
        // One key and two contacts: three entries, so nine header bytes and
        // ten more. Entry n's fields sit at 5n + 4, 5n + 5, and 5n + 7, the
        // key first.
        let mut report = [0_u8; 19];
        report[3] = 0x12;
        report[8] = 0x10;
        report[5 + 4] = 0x3f;
        report[5 + 5] = 0xef;
        report[5 + 7] = 0x01;
        report[10 + 4] = 0x20;
        report[10 + 5] = 0x17;
        report[10 + 7] = 0x10;
        bus.reads.push(report[..9].to_vec());
        bus.reads.push(report[9..].to_vec());
        let parsed = Cst3530::new(0x58)
            .expect("valid address")
            .read_report(&mut bus)
            .expect("report");
        let points = parsed.points();
        assert_eq!(points.len(), 2);
        assert_eq!((points[0].x, points[0].y), (0x13f, 0x0ef));
        assert_eq!((points[1].x, points[1].y), (0x020, 0x117));
        assert_eq!(bus.writes[0], [0xd0, 0x07, 0x00, 0x00]);
        assert_eq!(
            bus.writes.last().expect("acknowledgement"),
            &[0xd0, 0x00, 0x02, 0xab]
        );
    }

    #[test]
    fn a_report_without_contacts_is_empty_and_acknowledged() {
        let mut bus = Bus::default();
        bus.reads.push(std::vec![0; 9]);
        let parsed = Cst3530::new(0x58)
            .expect("valid address")
            .read_report(&mut bus)
            .expect("report");
        assert!(parsed.points().is_empty());
        assert_eq!(bus.writes.len(), 2);
    }
}

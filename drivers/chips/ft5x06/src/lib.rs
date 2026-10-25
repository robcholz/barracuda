//! Register-level FocalTech FT5x06-family touch-controller driver.
//!
//! The FT5206, FT6236, and FT6336U share this register map. The sequence and
//! report layout follow M5GFX's `Touch_FT5x06`: `DEVICE_MODE` (00h) set to
//! normal operation, the identity block from `CIPHER` (A3h) to `VENDID`
//! (A8h), `G_MODE` (A4h) set to polling, then `TD_STATUS` (02h) followed by
//! six bytes per contact from 03h.

#![no_std]

use embedded_hal::i2c::I2c;

const DEVICE_MODE: u8 = 0x00;
const TD_STATUS: u8 = 0x02;
const FIRST_POINT: u8 = 0x03;
const CIPHER: u8 = 0xa3;
const INTERRUPT_MODE: u8 = 0xa4;
const NORMAL_OPERATION: u8 = 0x00;
const POLLING: u8 = 0x00;
const POINT_BYTES: usize = 6;

/// The family's usual seven-bit address.
pub const DEFAULT_ADDRESS: u8 = 0x38;

/// Most contacts any family member reports; the FT6336U reports two.
pub const MAX_POINTS: usize = 5;

/// The identity block read at start-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Chip family (`CIPHER`, A3h).
    pub cipher: u8,
    /// Firmware version (`FIRMID`, A6h).
    pub firmware: u8,
    /// Panel vendor (`VENDID`, A8h); zero when no panel answers.
    pub vendor: u8,
}

/// One chip-native contact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    /// Controller-assigned touch identifier.
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

/// FT5x06 transport or identity failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// The controller answered with a zero vendor ID, as M5GFX rejects it.
    NotPresent,
}

/// An FT5x06-family controller at a caller-selected I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ft5x06 {
    address: u8,
}

impl Ft5x06 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Selects normal operation, reads the identity block, and selects the
    /// polling interrupt mode.
    ///
    /// # Errors
    ///
    /// Returns a bus error, or [`Error::NotPresent`] for a zero vendor ID.
    pub fn initialize<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Identity, Error<I2C::Error>> {
        i2c.write(self.address, &[DEVICE_MODE, NORMAL_OPERATION])
            .map_err(Error::Bus)?;
        let mut identity = [0; 6];
        i2c.write_read(self.address, &[CIPHER], &mut identity)
            .map_err(Error::Bus)?;
        if identity[5] == 0 {
            return Err(Error::NotPresent);
        }
        i2c.write(self.address, &[INTERRUPT_MODE, POLLING])
            .map_err(Error::Bus)?;
        Ok(Identity {
            cipher: identity[0],
            firmware: identity[3],
            vendor: identity[5],
        })
    }

    /// Reads the current contacts, at most `max_points` of them.
    ///
    /// # Errors
    ///
    /// Returns the bus error of a failed read.
    pub fn read_report<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        max_points: usize,
    ) -> Result<Report, I2C::Error> {
        let mut status = [0];
        i2c.write_read(self.address, &[TD_STATUS], &mut status)?;
        let count = usize::from(status[0] & 0x0f)
            .min(max_points)
            .min(MAX_POINTS);
        let mut report = Report {
            points: [Point::default(); MAX_POINTS],
            len: 0,
        };
        if count == 0 {
            return Ok(report);
        }
        let mut data = [0; MAX_POINTS * POINT_BYTES];
        let data = &mut data[..count.saturating_mul(POINT_BYTES)];
        i2c.write_read(self.address, &[FIRST_POINT], data)?;
        for (point, bytes) in report
            .points
            .iter_mut()
            .zip(data.as_chunks::<POINT_BYTES>().0)
        {
            *point = Point {
                id: bytes[2] >> 4,
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

    use super::{Error, Ft5x06};

    /// A register file that records writes and serves reads from a pointer.
    struct Registers {
        memory: [u8; 256],
        writes: Vec<(u8, u8)>,
    }

    impl Registers {
        fn new() -> Self {
            Self {
                memory: [0; 256],
                writes: Vec::new(),
            }
        }
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
            assert_eq!(address, 0x38);
            let mut pointer = 0_usize;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        pointer = usize::from(bytes[0]);
                        if let [register, value] = bytes[..] {
                            self.memory[usize::from(register)] = value;
                            self.writes.push((register, value));
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
    fn initializes_normal_mode_and_polling_after_reading_the_identity() {
        let mut bus = Registers::new();
        bus.memory[0xa3] = 0x64;
        bus.memory[0xa6] = 0x10;
        bus.memory[0xa8] = 0x11;
        bus.memory[0xa4] = 0x01;
        let chip = Ft5x06::new(0x38).expect("valid address");
        let identity = chip.initialize(&mut bus).expect("identity");
        assert_eq!(
            (identity.cipher, identity.firmware, identity.vendor),
            (0x64, 0x10, 0x11)
        );
        assert_eq!(bus.writes, [(0x00, 0x00), (0xa4, 0x00)]);
    }

    #[test]
    fn rejects_a_zero_vendor_id() {
        let mut bus = Registers::new();
        let chip = Ft5x06::new(0x38).expect("valid address");
        assert!(matches!(chip.initialize(&mut bus), Err(Error::NotPresent)));
    }

    #[test]
    fn decodes_two_contacts_and_limits_the_count() {
        let mut bus = Registers::new();
        bus.memory[0x02] = 0x02;
        // Contact 0: event 2, x 0x13f, id 0, y 0x0ef.
        bus.memory[0x03..0x09].copy_from_slice(&[0x81, 0x3f, 0x00, 0xef, 0x10, 0x00]);
        // Contact 1: x 0x020, id 1, y 0x117.
        bus.memory[0x09..0x0f].copy_from_slice(&[0x80, 0x20, 0x11, 0x17, 0x10, 0x00]);
        let chip = Ft5x06::new(0x38).expect("valid address");
        let report = chip.read_report(&mut bus, 2).expect("report");
        let points = report.points();
        assert_eq!(points.len(), 2);
        assert_eq!((points[0].id, points[0].x, points[0].y), (0, 0x13f, 0x0ef));
        assert_eq!((points[1].id, points[1].x, points[1].y), (1, 0x020, 0x117));
        assert_eq!(
            chip.read_report(&mut bus, 1)
                .expect("report")
                .points()
                .len(),
            1
        );

        bus.memory[0x02] = 0x0f;
        assert_eq!(
            chip.read_report(&mut bus, 2)
                .expect("report")
                .points()
                .len(),
            2
        );
        bus.memory[0x02] = 0x00;
        assert!(
            chip.read_report(&mut bus, 2)
                .expect("report")
                .points()
                .is_empty()
        );
    }
}

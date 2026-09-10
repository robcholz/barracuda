//! Register-level Hynitron CST816T touch-controller driver.

#![no_std]

use embedded_hal::i2c::I2c;

const GESTURE_ID: u8 = 0x01;
const CHIP_ID: u8 = 0xa7;
const REPORT_BYTES: usize = 6;

/// Default seven-bit address used by the CST816 family.
pub const DEFAULT_ADDRESS: u8 = 0x15;

/// One chip-native single-contact report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Gesture identifier reported by the controller.
    pub gesture: u8,
    /// Whether a contact is currently present.
    pub touched: bool,
    /// Horizontal coordinate when `touched` is true.
    pub x: u16,
    /// Vertical coordinate when `touched` is true.
    pub y: u16,
}

/// CST816T transport or report failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// The controller returned an impossible contact count.
    InvalidReport,
}

/// A CST816T-compatible controller at a caller-selected I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cst816t {
    address: u8,
}

impl Cst816t {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Reads the silicon identifier.
    pub fn chip_id<I2C: I2c>(&self, i2c: &mut I2C) -> Result<u8, I2C::Error> {
        let mut id = [0];
        i2c.write_read(self.address, &[CHIP_ID], &mut id)?;
        Ok(id[0])
    }

    /// Returns whether the device reports a known CST816-family identifier.
    pub fn is_present<I2C: I2c>(&self, i2c: &mut I2C) -> Result<bool, I2C::Error> {
        self.chip_id(i2c).map(|id| matches!(id, 0xb4 | 0xb5 | 0xb6))
    }

    /// Reads the latest gesture and single-contact coordinates.
    pub fn read_report<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Report, Error<I2C::Error>> {
        let mut data = [0; REPORT_BYTES];
        i2c.write_read(self.address, &[GESTURE_ID], &mut data)
            .map_err(Error::Bus)?;
        let count = data[1] & 0x0f;
        if count > 1 {
            return Err(Error::InvalidReport);
        }
        Ok(Report {
            gesture: data[0],
            touched: count == 1,
            x: u16::from(data[2] & 0x0f) << 8 | u16::from(data[3]),
            y: u16::from(data[4] & 0x0f) << 8 | u16::from(data[5]),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::Cst816t;

    struct Bus;

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn read(&mut self, _: u8, _: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn write(&mut self, _: u8, _: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn write_read(
            &mut self,
            _: u8,
            register: &[u8],
            output: &mut [u8],
        ) -> Result<(), Self::Error> {
            match register {
                [0xa7] => output.copy_from_slice(&[0xb4]),
                [0x01] => output.copy_from_slice(&[0x01, 0x01, 0x01, 0x23, 0x04, 0x56]),
                _ => output.fill(0),
            }
            Ok(())
        }

        fn transaction(&mut self, _: u8, _: &mut [Operation<'_>]) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn probes_and_decodes_contact() {
        let chip = Cst816t::new(0x15).expect("valid address");
        let mut bus = Bus;
        assert!(chip.is_present(&mut bus).expect("probe"));
        let report = chip.read_report(&mut bus).expect("report");
        assert!(report.touched);
        assert_eq!(report.x, 0x123);
        assert_eq!(report.y, 0x456);
    }
}

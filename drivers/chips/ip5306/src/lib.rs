//! Register-level driver for the read-only status of the Injoinic IP5306
//! power-bank SoC with I2C (the "IP5306_I2C" variant M5Stack fits).
//!
//! The IP5306 has no voltage or current ADC readable over I2C. It reports
//! the battery level in four 25 % steps, as its four indicator LEDs would
//! show it, and its charger state. Registers follow M5Stack's M5Unified
//! `IP5306_Class`:
//!
//! - `READ0` (`70h`) bit 3: charging is enabled and a supply is present;
//! - `READ1` (`71h`) bit 3: the charge has completed;
//! - `READ4` (`78h`) bits 7:4: the battery level, `0000` full, `1000` three
//!   quarters, `1100` half, `1110` a quarter, anything else empty.

#![no_std]

use embedded_hal::i2c::I2c;

/// The fixed seven-bit I2C address of the IP5306.
pub const ADDRESS: u8 = 0x75;

const READ_0: u8 = 0x70;
const READ_1: u8 = 0x71;
const READ_4: u8 = 0x78;
const CHARGER_ACTIVE: u8 = 1 << 3;
const CHARGE_COMPLETE: u8 = 1 << 3;

/// An IP5306 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ip5306 {
    address: u8,
}

impl Ip5306 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Reads the battery level: 0, 25, 50, 75, or 100 percent.
    ///
    /// # Errors
    ///
    /// Returns the bus error.
    pub fn battery_percent<I2C: I2c>(&self, i2c: &mut I2C) -> Result<u8, I2C::Error> {
        Ok(match self.read(i2c, READ_4)? >> 4 {
            0b0000 => 100,
            0b1000 => 75,
            0b1100 => 50,
            0b1110 => 25,
            _ => 0,
        })
    }

    /// Reports whether current flows into the battery: the charger runs and
    /// the charge has not completed. Both flags are read with single-byte
    /// transfers, as M5Unified does, because the datasheet does not specify
    /// address auto-increment.
    ///
    /// # Errors
    ///
    /// Returns the bus error.
    pub fn is_charging<I2C: I2c>(&self, i2c: &mut I2C) -> Result<bool, I2C::Error> {
        if self.read(i2c, READ_0)? & CHARGER_ACTIVE == 0 {
            return Ok(false);
        }
        Ok(self.read(i2c, READ_1)? & CHARGE_COMPLETE == 0)
    }

    fn read<I2C: I2c>(&self, i2c: &mut I2C, register: u8) -> Result<u8, I2C::Error> {
        let mut value = [0];
        i2c.write_read(self.address, &[register], &mut value)?;
        Ok(value[0])
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use core::convert::Infallible;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{ADDRESS, Ip5306};

    struct Chip {
        read_0: u8,
        read_1: u8,
        read_4: u8,
    }

    impl ErrorType for Chip {
        type Error = Infallible;
    }

    impl I2c for Chip {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, ADDRESS);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        assert_eq!(bytes.len(), 1, "the driver only reads");
                        register = bytes[0];
                    }
                    Operation::Read(bytes) => {
                        assert_eq!(bytes.len(), 1, "single-byte reads only");
                        bytes[0] = match register {
                            0x70 => self.read_0,
                            0x71 => self.read_1,
                            0x78 => self.read_4,
                            _ => 0,
                        };
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn decodes_the_four_level_gauge() {
        let chip = Ip5306::new(ADDRESS).expect("valid address");
        for (bits, percent) in [
            (0x00, 100),
            (0x80, 75),
            (0xc0, 50),
            (0xe0, 25),
            (0xf0, 0),
            (0x40, 0),
        ] {
            let mut bus = Chip {
                read_0: 0,
                read_1: 0,
                read_4: bits | 0x0f,
            };
            assert_eq!(chip.battery_percent(&mut bus).expect("level"), percent);
        }
    }

    #[test]
    fn a_completed_charge_is_not_charging() {
        let chip = Ip5306::new(ADDRESS).expect("valid address");
        let mut charging = Chip {
            read_0: 0x08,
            read_1: 0x00,
            read_4: 0,
        };
        assert!(chip.is_charging(&mut charging).expect("state"));
        let mut full = Chip {
            read_0: 0x08,
            read_1: 0x08,
            read_4: 0,
        };
        assert!(!chip.is_charging(&mut full).expect("state"));
        let mut unplugged = Chip {
            read_0: 0x00,
            read_1: 0x00,
            read_4: 0,
        };
        assert!(!chip.is_charging(&mut unplugged).expect("state"));
        assert!(Ip5306::new(0xea).is_none());
    }
}

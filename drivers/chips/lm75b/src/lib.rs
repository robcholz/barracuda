//! Register-level driver for the NXP LM75B digital temperature sensor.
//!
//! Registers follow the NXP LM75B datasheet: pointer `00h` is the
//! temperature, `01h` the configuration, whose bit 0 shuts the converter
//! down. The temperature is an 11-bit two's-complement
//! value in the top bits of its two big-endian bytes, 0.125 °C per step.
//!
//! The upstream `lm75` crate is not used: its LM75 device type masks the
//! reading to the original LM75's 9 bits, and only its PCT2075 type keeps
//! all 11, and it converts through floating point.

#![no_std]

use embedded_hal::i2c::I2c;

/// The address with A2, A1, and A0 low.
pub const DEFAULT_ADDRESS: u8 = 0x48;

const TEMPERATURE: u8 = 0x00;
const CONFIGURATION: u8 = 0x01;
/// Normal operation: converter running, comparator mode, OS active low,
/// fault queue of one (the power-on default).
const NORMAL_OPERATION: u8 = 0x00;

/// An LM75B at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lm75b {
    address: u8,
}

impl Lm75b {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Selects continuous conversion, leaving shutdown if a previous
    /// firmware entered it, and checks the register reads back.
    ///
    /// # Errors
    ///
    /// Returns the bus error.
    pub fn start<I2C: I2c>(&self, i2c: &mut I2C) -> Result<bool, I2C::Error> {
        i2c.write(self.address, &[CONFIGURATION, NORMAL_OPERATION])?;
        let mut configuration = [0xff];
        i2c.write_read(self.address, &[CONFIGURATION], &mut configuration)?;
        // Bits 7 to 5 are reserved and read as zero.
        Ok(configuration[0] & 0x1f == NORMAL_OPERATION)
    }

    /// Reads the latest conversion in milli-degrees Celsius.
    ///
    /// # Errors
    ///
    /// Returns the bus error.
    pub fn temperature_millicelsius<I2C: I2c>(&self, i2c: &mut I2C) -> Result<i32, I2C::Error> {
        let mut data = [0; 2];
        i2c.write_read(self.address, &[TEMPERATURE], &mut data)?;
        Ok(decode(data))
    }
}

/// Decodes the temperature register: the top 11 bits, 125 m°C per step.
#[must_use]
pub fn decode(data: [u8; 2]) -> i32 {
    let steps = i16::from_be_bytes(data) >> 5;
    i32::from(steps).saturating_mul(125)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::{vec, vec::Vec};

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{DEFAULT_ADDRESS, Lm75b, decode};

    struct Sensor {
        registers: [u8; 4],
        pointer: usize,
        writes: Vec<Vec<u8>>,
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
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        self.pointer = usize::from(bytes[0]);
                        if let [_, value] = bytes[..] {
                            self.registers[self.pointer + 1] = value;
                            self.writes.push(bytes.to_vec());
                        }
                    }
                    Operation::Read(buffer) => {
                        // Temperature at bytes 0-1, configuration at 2.
                        let start = if self.pointer == 0 { 0 } else { 2 };
                        buffer.copy_from_slice(&self.registers[start..start + buffer.len()]);
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn leaves_shutdown_and_reads_the_temperature() {
        // +25 degrees Celsius is 200 steps, stored as 1900h; the shutdown
        // bit is set, as an earlier firmware may leave it.
        let mut bus = Sensor {
            registers: [0x19, 0x00, 0x01, 0x00],
            pointer: 0,
            writes: Vec::new(),
        };
        let sensor = Lm75b::new(DEFAULT_ADDRESS).expect("valid address");
        assert!(sensor.start(&mut bus).expect("start"));
        assert_eq!(bus.writes, [vec![0x01, 0x00]]);
        assert_eq!(
            sensor.temperature_millicelsius(&mut bus).expect("read"),
            25_000
        );
    }

    #[test]
    fn decodes_signed_steps() {
        // +127, +0.125, -0.125, and -54.875 degrees Celsius.
        assert_eq!(decode([0x7f, 0x00]), 127_000);
        assert_eq!(decode([0x00, 0x20]), 125);
        assert_eq!(decode([0xff, 0xe0]), -125);
        assert_eq!(decode([0xc9, 0x20]), -54_875);
        assert!(Lm75b::new(0x80).is_none());
    }
}

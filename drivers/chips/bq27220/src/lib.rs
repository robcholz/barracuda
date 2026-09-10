//! Register-level Texas Instruments BQ27220 battery fuel-gauge driver.

#![no_std]

use embedded_hal::i2c::I2c;

const TEMPERATURE: u8 = 0x06;
const VOLTAGE: u8 = 0x08;
const CURRENT: u8 = 0x0c;
const REMAINING_CAPACITY: u8 = 0x10;
const FULL_CHARGE_CAPACITY: u8 = 0x12;
const STATE_OF_CHARGE: u8 = 0x2c;

/// Default seven-bit address of the BQ27220.
pub const DEFAULT_ADDRESS: u8 = 0x55;

/// Raw battery telemetry in the units defined by the gauge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measurement {
    /// Battery voltage in millivolts.
    pub voltage_millivolts: u16,
    /// Signed instantaneous current in milliamps.
    pub current_milliamps: i16,
    /// Temperature in tenths of kelvin.
    pub temperature_decikelvin: u16,
    /// Remaining capacity in milliamp-hours.
    pub remaining_capacity_milliamp_hours: u16,
    /// Full-charge capacity in milliamp-hours.
    pub full_charge_capacity_milliamp_hours: u16,
    /// State of charge in percent.
    pub state_of_charge_percent: u16,
}

/// A BQ27220 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bq27220 {
    address: u8,
}

impl Bq27220 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Reads one coherent set of standard-command battery measurements.
    pub fn measure<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Measurement, I2C::Error> {
        Ok(Measurement {
            voltage_millivolts: self.read_word(i2c, VOLTAGE)?,
            current_milliamps: self.read_word(i2c, CURRENT)? as i16,
            temperature_decikelvin: self.read_word(i2c, TEMPERATURE)?,
            remaining_capacity_milliamp_hours: self.read_word(i2c, REMAINING_CAPACITY)?,
            full_charge_capacity_milliamp_hours: self.read_word(i2c, FULL_CHARGE_CAPACITY)?,
            state_of_charge_percent: self.read_word(i2c, STATE_OF_CHARGE)?,
        })
    }

    /// Reads the battery voltage as a low-impact presence probe.
    pub fn voltage_millivolts<I2C: I2c>(&self, i2c: &mut I2C) -> Result<u16, I2C::Error> {
        self.read_word(i2c, VOLTAGE)
    }

    fn read_word<I2C: I2c>(&self, i2c: &mut I2C, command: u8) -> Result<u16, I2C::Error> {
        let mut value = [0; 2];
        i2c.write_read(self.address, &[command], &mut value)?;
        Ok(u16::from_le_bytes(value))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::Bq27220;

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
            command: &[u8],
            output: &mut [u8],
        ) -> Result<(), Self::Error> {
            let value = match command[0] {
                0x06 => 2_981,
                0x08 => 3_925,
                0x0c => (-125_i16) as u16,
                0x10 => 350,
                0x12 => 700,
                0x2c => 50,
                _ => 0,
            };
            output.copy_from_slice(&value.to_le_bytes());
            Ok(())
        }

        fn transaction(&mut self, _: u8, _: &mut [Operation<'_>]) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn reads_standard_commands_as_little_endian_words() {
        let chip = Bq27220::new(0x55).expect("valid address");
        let measurement = chip.measure(&mut Bus).expect("measurement");
        assert_eq!(measurement.voltage_millivolts, 3_925);
        assert_eq!(measurement.current_milliamps, -125);
        assert_eq!(measurement.state_of_charge_percent, 50);
    }
}

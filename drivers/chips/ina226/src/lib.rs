//! Register-level INA226 current and power monitor driver.

#![no_std]

use embedded_hal::i2c::I2c;

const SHUNT_VOLTAGE: u8 = 0x01;
const BUS_VOLTAGE: u8 = 0x02;
const MANUFACTURER_ID: u8 = 0xfe;
const DIE_ID: u8 = 0xff;

/// Raw identity registers returned by an INA226-compatible device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Manufacturer identifier.
    pub manufacturer: u16,
    /// Die identifier and revision.
    pub die: u16,
}

impl Identity {
    /// Returns whether the identity is the Texas Instruments INA226 family.
    #[must_use]
    pub const fn is_ina226(self) -> bool {
        self.manufacturer == 0x5449 && self.die & 0xfff0 == 0x2260
    }
}

/// An INA226 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ina226 {
    address: u8,
}

impl Ina226 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Reads manufacturer and die identifiers.
    pub fn identity<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Identity, I2C::Error> {
        Ok(Identity {
            manufacturer: self.read_register(i2c, MANUFACTURER_ID)?,
            die: self.read_register(i2c, DIE_ID)?,
        })
    }

    /// Reads the raw unsigned bus-voltage register.
    pub fn bus_voltage_raw<I2C: I2c>(&self, i2c: &mut I2C) -> Result<u16, I2C::Error> {
        self.read_register(i2c, BUS_VOLTAGE)
    }

    /// Reads the raw signed shunt-voltage register.
    pub fn shunt_voltage_raw<I2C: I2c>(&self, i2c: &mut I2C) -> Result<i16, I2C::Error> {
        self.read_register(i2c, SHUNT_VOLTAGE)
            .map(|value| value as i16)
    }

    fn read_register<I2C: I2c>(&self, i2c: &mut I2C, register: u8) -> Result<u16, I2C::Error> {
        let mut value = [0; 2];
        i2c.write_read(self.address, &[register], &mut value)?;
        Ok(u16::from_be_bytes(value))
    }
}

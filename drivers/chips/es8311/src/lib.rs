//! Register-level ES8311 audio-codec driver.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

const INITIAL_REGISTERS: &[(u8, u8)] = &[
    (0x00, 0x80),
    (0x01, 0xba),
    (0x02, 0x18),
    (0x09, 0x0c),
    (0x0a, 0x0c),
    (0x0d, 0x01),
    (0x0e, 0x02),
    (0x12, 0x00),
    (0x14, 0x1a),
    (0x15, 0x40),
    (0x17, 0xbf),
    (0x32, 0xbf),
    (0x37, 0x08),
    (0x44, 0x08),
    (0x45, 0x00),
];

/// An ES8311 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Es8311 {
    address: u8,
}

impl Es8311 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Resets and programs the codec's 16-bit stereo signal path.
    pub fn initialize<I2C: I2c, DELAY: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut DELAY,
    ) -> Result<(), I2C::Error> {
        self.write_register(i2c, 0x00, 0x1f)?;
        delay.delay_ms(20);
        self.write_register(i2c, 0x00, 0x00)?;
        for &(register, value) in INITIAL_REGISTERS {
            self.write_register(i2c, register, value)?;
        }
        Ok(())
    }

    /// Writes the DAC volume register.
    pub fn set_dac_volume<I2C: I2c>(&self, i2c: &mut I2C, volume: u8) -> Result<(), I2C::Error> {
        self.write_register(i2c, 0x32, volume)
    }

    fn write_register<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        register: u8,
        value: u8,
    ) -> Result<(), I2C::Error> {
        i2c.write(self.address, &[register, value])
    }
}

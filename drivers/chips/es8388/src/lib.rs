//! Register-level ES8388 audio-codec driver.

#![no_std]

use embedded_hal::i2c::I2c;

const INITIAL_REGISTERS: &[(u8, u8)] = &[
    (0x19, 0x04),
    (0x01, 0x50),
    (0x02, 0x00),
    (0x35, 0xa0),
    (0x37, 0xd0),
    (0x39, 0xd0),
    (0x08, 0x00),
    (0x04, 0xc0),
    (0x00, 0x12),
    (0x17, 0x18),
    (0x18, 0x02),
    (0x26, 0x00),
    (0x27, 0x90),
    (0x2a, 0x90),
    (0x2b, 0x80),
    (0x2d, 0x00),
    (0x1a, 0x00),
    (0x1b, 0x00),
    (0x2e, 0x1e),
    (0x2f, 0x1e),
    (0x30, 0x00),
    (0x31, 0x00),
    (0x04, 0x3c),
    (0x02, 0xf0),
    (0x02, 0x00),
    (0x19, 0x00),
];

/// An ES8388 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Es8388 {
    address: u8,
}

impl Es8388 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Programs the I2S-slave DAC path for 16-bit stereo and a 256-fs MCLK.
    pub fn initialize<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), I2C::Error> {
        for &(register, value) in INITIAL_REGISTERS {
            self.write_register(i2c, register, value)?;
        }
        Ok(())
    }

    /// Sets the left and right DAC attenuation registers.
    pub fn set_dac_attenuation<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        attenuation: u8,
    ) -> Result<(), I2C::Error> {
        self.write_register(i2c, 0x1a, attenuation)?;
        self.write_register(i2c, 0x1b, attenuation)
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

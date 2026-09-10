//! Register-level ES7210 microphone-ADC driver.

#![no_std]

use embedded_hal::i2c::I2c;

const INITIAL_REGISTERS: &[(u8, u8)] = &[
    (0x00, 0xff),
    (0x00, 0x41),
    (0x01, 0x3f),
    (0x09, 0x30),
    (0x0a, 0x30),
    (0x23, 0x2a),
    (0x22, 0x0a),
    (0x20, 0x0a),
    (0x21, 0x2a),
    (0x08, 0x00),
    (0x40, 0x43),
    (0x41, 0x70),
    (0x42, 0x70),
    (0x07, 0x20),
    (0x02, 0xc1),
    (0x43, 0x1a),
    (0x44, 0x1a),
    (0x45, 0x00),
    (0x46, 0x00),
    (0x4b, 0x00),
    (0x4c, 0xff),
    (0x12, 0x00),
    (0x11, 0x60),
    (0x01, 0x34),
    (0x06, 0x00),
    (0x47, 0x08),
    (0x48, 0x08),
    (0x49, 0x08),
    (0x4a, 0x08),
    (0x00, 0x71),
    (0x00, 0x41),
];

/// An ES7210 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Es7210 {
    address: u8,
}

impl Es7210 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Programs slave-mode MIC1/MIC2 capture for 16-bit, two-channel I2S.
    pub fn initialize<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), I2C::Error> {
        for &(register, value) in INITIAL_REGISTERS {
            i2c.write(self.address, &[register, value])?;
        }
        Ok(())
    }
}

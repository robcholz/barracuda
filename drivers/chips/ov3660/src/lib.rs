//! Register-level OV3660 image-sensor driver.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

#[derive(Clone, Copy)]
enum RegisterWrite {
    Value(u16, u8),
    DelayMs(u32),
}

const QVGA_JPEG_REGISTERS: &[RegisterWrite] = &[
    RegisterWrite::Value(0x3008, 0x82),
    RegisterWrite::DelayMs(10),
    RegisterWrite::Value(0x3103, 0x13),
    RegisterWrite::Value(0x3008, 0x42),
    RegisterWrite::Value(0x3017, 0xff),
    RegisterWrite::Value(0x3018, 0xff),
    RegisterWrite::Value(0x302c, 0xc3),
    RegisterWrite::Value(0x4740, 0x21),
    RegisterWrite::Value(0x3611, 0x01),
    RegisterWrite::Value(0x3612, 0x2d),
    RegisterWrite::Value(0x3032, 0x00),
    RegisterWrite::Value(0x3614, 0x80),
    RegisterWrite::Value(0x3618, 0x00),
    RegisterWrite::Value(0x3619, 0x75),
    RegisterWrite::Value(0x3622, 0x80),
    RegisterWrite::Value(0x3623, 0x00),
    RegisterWrite::Value(0x3624, 0x03),
    RegisterWrite::Value(0x3630, 0x52),
    RegisterWrite::Value(0x3632, 0x07),
    RegisterWrite::Value(0x3633, 0xd2),
    RegisterWrite::Value(0x3704, 0x80),
    RegisterWrite::Value(0x3708, 0x66),
    RegisterWrite::Value(0x3709, 0x12),
    RegisterWrite::Value(0x370b, 0x12),
    RegisterWrite::Value(0x3717, 0x00),
    RegisterWrite::Value(0x371b, 0x60),
    RegisterWrite::Value(0x371c, 0x00),
    RegisterWrite::Value(0x3901, 0x13),
    RegisterWrite::Value(0x3600, 0x08),
    RegisterWrite::Value(0x3620, 0x43),
    RegisterWrite::Value(0x3702, 0x20),
    RegisterWrite::Value(0x3739, 0x48),
    RegisterWrite::Value(0x3730, 0x20),
    RegisterWrite::Value(0x370c, 0x0c),
    RegisterWrite::Value(0x3a18, 0x00),
    RegisterWrite::Value(0x3a19, 0xf8),
    RegisterWrite::Value(0x3000, 0x10),
    RegisterWrite::Value(0x3004, 0xef),
    RegisterWrite::Value(0x6700, 0x05),
    RegisterWrite::Value(0x6701, 0x19),
    RegisterWrite::Value(0x6702, 0xfd),
    RegisterWrite::Value(0x6703, 0xd1),
    RegisterWrite::Value(0x6704, 0xff),
    RegisterWrite::Value(0x6705, 0xff),
    RegisterWrite::Value(0x3c01, 0x80),
    RegisterWrite::Value(0x3c00, 0x04),
    RegisterWrite::Value(0x3a08, 0x00),
    RegisterWrite::Value(0x3a09, 0x62),
    RegisterWrite::Value(0x3a0e, 0x08),
    RegisterWrite::Value(0x3a0a, 0x00),
    RegisterWrite::Value(0x3a0b, 0x52),
    RegisterWrite::Value(0x3a0d, 0x09),
    RegisterWrite::Value(0x3a00, 0x3a),
    RegisterWrite::Value(0x3a14, 0x09),
    RegisterWrite::Value(0x3a15, 0x30),
    RegisterWrite::Value(0x3a02, 0x09),
    RegisterWrite::Value(0x3a03, 0x30),
    RegisterWrite::Value(0x440e, 0x08),
    RegisterWrite::Value(0x4520, 0x0b),
    RegisterWrite::Value(0x460b, 0x37),
    RegisterWrite::Value(0x4713, 0x02),
    RegisterWrite::Value(0x471c, 0xd0),
    RegisterWrite::Value(0x5086, 0x00),
    RegisterWrite::Value(0x5002, 0x00),
    RegisterWrite::Value(0x501f, 0x00),
    RegisterWrite::Value(0x3008, 0x02),
    RegisterWrite::Value(0x5180, 0xff),
    RegisterWrite::Value(0x5181, 0xf2),
    RegisterWrite::Value(0x5182, 0x00),
    RegisterWrite::Value(0x5183, 0x14),
    RegisterWrite::Value(0x5184, 0x25),
    RegisterWrite::Value(0x5185, 0x24),
    RegisterWrite::Value(0x5186, 0x16),
    RegisterWrite::Value(0x5187, 0x16),
    RegisterWrite::Value(0x5188, 0x16),
    RegisterWrite::Value(0x5189, 0x68),
    RegisterWrite::Value(0x518a, 0x60),
    RegisterWrite::Value(0x518b, 0xe0),
    RegisterWrite::Value(0x518c, 0xb2),
    RegisterWrite::Value(0x518d, 0x42),
    RegisterWrite::Value(0x518e, 0x35),
    RegisterWrite::Value(0x518f, 0x56),
    RegisterWrite::Value(0x5190, 0x56),
    RegisterWrite::Value(0x5191, 0xf8),
    RegisterWrite::Value(0x5192, 0x04),
    RegisterWrite::Value(0x5193, 0x70),
    RegisterWrite::Value(0x5194, 0xf0),
    RegisterWrite::Value(0x5195, 0xf0),
    RegisterWrite::Value(0x5196, 0x03),
    RegisterWrite::Value(0x5197, 0x01),
    RegisterWrite::Value(0x5198, 0x04),
    RegisterWrite::Value(0x5199, 0x12),
    RegisterWrite::Value(0x519a, 0x04),
    RegisterWrite::Value(0x519b, 0x00),
    RegisterWrite::Value(0x519c, 0x06),
    RegisterWrite::Value(0x519d, 0x82),
    RegisterWrite::Value(0x519e, 0x38),
    RegisterWrite::Value(0x5381, 0x1d),
    RegisterWrite::Value(0x5382, 0x60),
    RegisterWrite::Value(0x5383, 0x03),
    RegisterWrite::Value(0x5384, 0x0c),
    RegisterWrite::Value(0x5385, 0x78),
    RegisterWrite::Value(0x5386, 0x84),
    RegisterWrite::Value(0x5387, 0x7d),
    RegisterWrite::Value(0x5388, 0x6b),
    RegisterWrite::Value(0x5389, 0x12),
    RegisterWrite::Value(0x538a, 0x01),
    RegisterWrite::Value(0x538b, 0x98),
    RegisterWrite::Value(0x5480, 0x01),
    RegisterWrite::Value(0x5000, 0xa7),
    RegisterWrite::Value(0x5800, 0x0c),
    RegisterWrite::Value(0x5801, 0x09),
    RegisterWrite::Value(0x5802, 0x0c),
    RegisterWrite::Value(0x5803, 0x0c),
    RegisterWrite::Value(0x5804, 0x0d),
    RegisterWrite::Value(0x5805, 0x17),
    RegisterWrite::Value(0x5806, 0x06),
    RegisterWrite::Value(0x5807, 0x05),
    RegisterWrite::Value(0x5808, 0x04),
    RegisterWrite::Value(0x5809, 0x06),
    RegisterWrite::Value(0x580a, 0x09),
    RegisterWrite::Value(0x580b, 0x0e),
    RegisterWrite::Value(0x580c, 0x05),
    RegisterWrite::Value(0x580d, 0x01),
    RegisterWrite::Value(0x580e, 0x01),
    RegisterWrite::Value(0x580f, 0x01),
    RegisterWrite::Value(0x5810, 0x05),
    RegisterWrite::Value(0x5811, 0x0d),
    RegisterWrite::Value(0x5812, 0x05),
    RegisterWrite::Value(0x5813, 0x01),
    RegisterWrite::Value(0x5814, 0x01),
    RegisterWrite::Value(0x5815, 0x01),
    RegisterWrite::Value(0x5816, 0x05),
    RegisterWrite::Value(0x5817, 0x0d),
    RegisterWrite::Value(0x5818, 0x08),
    RegisterWrite::Value(0x5819, 0x06),
    RegisterWrite::Value(0x581a, 0x05),
    RegisterWrite::Value(0x581b, 0x07),
    RegisterWrite::Value(0x581c, 0x0b),
    RegisterWrite::Value(0x581d, 0x0d),
    RegisterWrite::Value(0x581e, 0x12),
    RegisterWrite::Value(0x581f, 0x0d),
    RegisterWrite::Value(0x5820, 0x0e),
    RegisterWrite::Value(0x5821, 0x10),
    RegisterWrite::Value(0x5822, 0x10),
    RegisterWrite::Value(0x5823, 0x1e),
    RegisterWrite::Value(0x5824, 0x53),
    RegisterWrite::Value(0x5825, 0x15),
    RegisterWrite::Value(0x5826, 0x05),
    RegisterWrite::Value(0x5827, 0x14),
    RegisterWrite::Value(0x5828, 0x54),
    RegisterWrite::Value(0x5829, 0x25),
    RegisterWrite::Value(0x582a, 0x33),
    RegisterWrite::Value(0x582b, 0x33),
    RegisterWrite::Value(0x582c, 0x34),
    RegisterWrite::Value(0x582d, 0x16),
    RegisterWrite::Value(0x582e, 0x24),
    RegisterWrite::Value(0x582f, 0x41),
    RegisterWrite::Value(0x5830, 0x50),
    RegisterWrite::Value(0x5831, 0x42),
    RegisterWrite::Value(0x5832, 0x15),
    RegisterWrite::Value(0x5833, 0x25),
    RegisterWrite::Value(0x5834, 0x34),
    RegisterWrite::Value(0x5835, 0x33),
    RegisterWrite::Value(0x5836, 0x24),
    RegisterWrite::Value(0x5837, 0x26),
    RegisterWrite::Value(0x5838, 0x54),
    RegisterWrite::Value(0x5839, 0x25),
    RegisterWrite::Value(0x583a, 0x15),
    RegisterWrite::Value(0x583b, 0x25),
    RegisterWrite::Value(0x583c, 0x53),
    RegisterWrite::Value(0x583d, 0xcf),
    RegisterWrite::Value(0x3a0f, 0x3b),
    RegisterWrite::Value(0x3a10, 0x32),
    RegisterWrite::Value(0x3a1b, 0x3b),
    RegisterWrite::Value(0x3a1e, 0x32),
    RegisterWrite::Value(0x3a11, 0x76),
    RegisterWrite::Value(0x3a1f, 0x19),
    RegisterWrite::Value(0x5302, 0x28),
    RegisterWrite::Value(0x5303, 0x20),
    RegisterWrite::Value(0x5306, 0x1c),
    RegisterWrite::Value(0x5307, 0x28),
    RegisterWrite::Value(0x4002, 0xc5),
    RegisterWrite::Value(0x4003, 0x81),
    RegisterWrite::Value(0x4005, 0x12),
    RegisterWrite::Value(0x5688, 0x11),
    RegisterWrite::Value(0x5689, 0x11),
    RegisterWrite::Value(0x568a, 0x11),
    RegisterWrite::Value(0x568b, 0x11),
    RegisterWrite::Value(0x568c, 0x11),
    RegisterWrite::Value(0x568d, 0x11),
    RegisterWrite::Value(0x568e, 0x11),
    RegisterWrite::Value(0x568f, 0x11),
    RegisterWrite::Value(0x5580, 0x06),
    RegisterWrite::Value(0x5588, 0x00),
    RegisterWrite::Value(0x5583, 0x40),
    RegisterWrite::Value(0x5584, 0x2c),
    RegisterWrite::Value(0x5001, 0x83),
    RegisterWrite::Value(0x501f, 0x00),
    RegisterWrite::Value(0x4300, 0x30),
    RegisterWrite::Value(0x3002, 0x00),
    RegisterWrite::Value(0x3006, 0xff),
    RegisterWrite::Value(0x471c, 0x50),
    RegisterWrite::Value(0x3800, 0x00),
    RegisterWrite::Value(0x3801, 0x00),
    RegisterWrite::Value(0x3802, 0x00),
    RegisterWrite::Value(0x3803, 0x00),
    RegisterWrite::Value(0x3804, 0x08),
    RegisterWrite::Value(0x3805, 0x1f),
    RegisterWrite::Value(0x3806, 0x06),
    RegisterWrite::Value(0x3807, 0x0b),
    RegisterWrite::Value(0x3808, 0x01),
    RegisterWrite::Value(0x3809, 0x40),
    RegisterWrite::Value(0x380a, 0x00),
    RegisterWrite::Value(0x380b, 0xf0),
    RegisterWrite::Value(0x380c, 0x08),
    RegisterWrite::Value(0x380d, 0xfc),
    RegisterWrite::Value(0x380e, 0x03),
    RegisterWrite::Value(0x380f, 0x0f),
    RegisterWrite::Value(0x3810, 0x00),
    RegisterWrite::Value(0x3811, 0x08),
    RegisterWrite::Value(0x3812, 0x00),
    RegisterWrite::Value(0x3813, 0x02),
    RegisterWrite::Value(0x3820, 0x01),
    RegisterWrite::Value(0x3821, 0x21),
    RegisterWrite::Value(0x4514, 0xaa),
    RegisterWrite::Value(0x4520, 0x0b),
    RegisterWrite::Value(0x3814, 0x31),
    RegisterWrite::Value(0x3815, 0x31),
    RegisterWrite::Value(0x303a, 0x00),
    RegisterWrite::Value(0x303b, 0x1e),
    RegisterWrite::Value(0x303c, 0x11),
    RegisterWrite::Value(0x303d, 0x30),
    RegisterWrite::Value(0x3824, 0x0a),
    RegisterWrite::Value(0x460c, 0x22),
];

/// An OV3660 at a caller-selected seven-bit SCCB address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ov3660 {
    address: u8,
}

impl Ov3660 {
    /// Creates a driver when `address` is a valid seven-bit SCCB address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Reads the 16-bit product identity.
    pub fn product_id<I2C: I2c>(&self, i2c: &mut I2C) -> Result<u16, I2C::Error> {
        Ok(u16::from(self.read_register(i2c, 0x300a)?) << 8
            | u16::from(self.read_register(i2c, 0x300b)?))
    }

    /// Programs QVGA JPEG output.
    pub fn initialize_qvga_jpeg<I2C: I2c, DELAY: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut DELAY,
    ) -> Result<(), I2C::Error> {
        for command in QVGA_JPEG_REGISTERS {
            match *command {
                RegisterWrite::Value(register, value) => {
                    self.write_register(i2c, register, value)?
                }
                RegisterWrite::DelayMs(milliseconds) => delay.delay_ms(milliseconds),
            }
        }
        delay.delay_ms(100);
        Ok(())
    }

    fn write_register<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        register: u16,
        value: u8,
    ) -> Result<(), I2C::Error> {
        let [high, low] = register.to_be_bytes();
        i2c.write(self.address, &[high, low, value])
    }

    fn read_register<I2C: I2c>(&self, i2c: &mut I2C, register: u16) -> Result<u8, I2C::Error> {
        let mut value = [0];
        i2c.write_read(self.address, &register.to_be_bytes(), &mut value)?;
        Ok(value[0])
    }
}

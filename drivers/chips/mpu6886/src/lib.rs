//! Register-level driver for the TDK InvenSense MPU6886 six-axis sensor and
//! the MPU9250 and MPU6050 that share its accelerometer and gyroscope
//! registers.
//!
//! Registers follow the MPU6886 datasheet (DS-000193, register map section
//! 10) and M5Stack's M5Unified `MPU6886_Class`, which drives all three parts
//! with one sequence and tells them apart by `WHO_AM_I` (`75h`): `19h` is an
//! MPU6886, `71h` an MPU9250, and `68h` an MPU6050. Samples are the
//! big-endian accelerometer, temperature, and gyroscope words from `3Bh`.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// The address with AD0 low, as every M5Stack Board wires it.
pub const DEFAULT_ADDRESS: u8 = 0x68;

const SAMPLE_RATE_DIVIDER: u8 = 0x19;
const CONFIG: u8 = 0x1a;
const GYRO_CONFIG: u8 = 0x1b;
const ACCEL_CONFIG: u8 = 0x1c;
const ACCEL_CONFIG_2: u8 = 0x1d;
const FIFO_ENABLE: u8 = 0x23;
const INT_PIN_CONFIG: u8 = 0x37;
const INT_ENABLE: u8 = 0x38;
const ACCEL_XOUT_H: u8 = 0x3b;
const USER_CONTROL: u8 = 0x6a;
const POWER_MANAGEMENT_1: u8 = 0x6b;
const WHO_AM_I: u8 = 0x75;

/// `PWR_MGMT_1` DEVICE_RESET.
const DEVICE_RESET: u8 = 0x80;
/// `PWR_MGMT_1` CLKSEL 1: the gyroscope PLL when ready, else the oscillator.
const AUTO_CLOCK: u8 = 0x01;
/// `CONFIG` DLPF_CFG 1: 176 Hz gyroscope bandwidth with a 1 kHz internal
/// rate, so `SMPLRT_DIV` divides 1 kHz.
const LOW_PASS_176_HZ: u8 = 0x01;
/// `INT_PIN_CFG`: active-low, open-drain interrupt, as M5Unified sets it.
const INTERRUPT_ACTIVE_LOW_OPEN_DRAIN: u8 = 0xc0;
/// Time the part needs after DEVICE_RESET before it takes register writes.
const RESET_MS: u32 = 100;
/// M5Unified retries each configuration write until it reads back.
const WRITE_ATTEMPTS: u8 = 16;

/// Which part answered `WHO_AM_I`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// MPU6886, `WHO_AM_I` `19h`.
    Mpu6886,
    /// MPU9250, `WHO_AM_I` `71h`: an MPU6500 with an AK8963 magnetometer.
    Mpu9250,
    /// MPU6050, `WHO_AM_I` `68h`.
    Mpu6050,
}

impl Variant {
    /// Identifies a `WHO_AM_I` value.
    #[must_use]
    pub const fn from_who_am_i(value: u8) -> Option<Self> {
        match value {
            0x19 => Some(Self::Mpu6886),
            0x71 => Some(Self::Mpu9250),
            0x68 => Some(Self::Mpu6050),
            _ => None,
        }
    }

    /// Converts a raw temperature word into milli-degrees Celsius.
    ///
    /// MPU6886: 326.8 LSB/°C at 25 °C; MPU9250: 333.87 LSB/°C at 21 °C;
    /// MPU6050: 340 LSB/°C at 36.53 °C (their datasheets' `TEMP_OUT`).
    #[must_use]
    pub fn temperature_millicelsius(self, raw: i16) -> i32 {
        let raw = i64::from(raw);
        let millicelsius = match self {
            Self::Mpu6886 => raw
                .saturating_mul(10_000)
                .checked_div(3_268)
                .unwrap_or_default()
                .saturating_add(25_000),
            Self::Mpu9250 => raw
                .saturating_mul(100_000)
                .checked_div(33_387)
                .unwrap_or_default()
                .saturating_add(21_000),
            Self::Mpu6050 => raw
                .saturating_mul(100)
                .checked_div(34)
                .unwrap_or_default()
                .saturating_add(36_530),
        };
        i32::try_from(millicelsius).unwrap_or_default()
    }
}

/// Accelerometer full scale, `ACCEL_CONFIG` bits 4:3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccelRange {
    /// ±2 g.
    G2 = 0,
    /// ±4 g.
    G4 = 1,
    /// ±8 g.
    G8 = 2,
    /// ±16 g.
    G16 = 3,
}

/// Gyroscope full scale, `GYRO_CONFIG` bits 4:3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GyroRange {
    /// ±250 °/s.
    Dps250 = 0,
    /// ±500 °/s.
    Dps500 = 1,
    /// ±1000 °/s.
    Dps1000 = 2,
    /// ±2000 °/s.
    Dps2000 = 3,
}

/// One raw sample in the sensor's own axes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawSample {
    /// Accelerometer X, Y, Z.
    pub acceleration: [i16; 3],
    /// Die temperature word.
    pub temperature: i16,
    /// Gyroscope X, Y, Z.
    pub angular_velocity: [i16; 3],
}

/// MPU6886 transport or identity failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// `WHO_AM_I` named no supported part.
    UnknownDevice(u8),
    /// A configuration register did not read back what was written.
    WriteNotApplied(u8),
}

/// An MPU6886-family sensor at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mpu6886 {
    address: u8,
}

impl Mpu6886 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Reads `WHO_AM_I` and names the part, or `None` for another chip.
    ///
    /// # Errors
    ///
    /// Returns the bus error when nothing acknowledges the read.
    pub fn identify<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Option<Variant>, I2C::Error> {
        Ok(Variant::from_who_am_i(self.read_register(i2c, WHO_AM_I)?))
    }

    /// Resets the part and configures continuous accelerometer and
    /// gyroscope sampling at 1 kHz / (1 + `divider`) with a 176 Hz filter,
    /// interrupts and FIFO off, following M5Unified `MPU6886_Class::begin`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownDevice`] when another chip answers, a bus
    /// error, or [`Error::WriteNotApplied`] when a register keeps refusing
    /// its value.
    pub fn initialize<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
        accel: AccelRange,
        gyro: GyroRange,
        divider: u8,
    ) -> Result<Variant, Error<I2C::Error>> {
        let who_am_i = self.read_register(i2c, WHO_AM_I).map_err(Error::Bus)?;
        let variant = Variant::from_who_am_i(who_am_i).ok_or(Error::UnknownDevice(who_am_i))?;
        self.write_register(i2c, POWER_MANAGEMENT_1, DEVICE_RESET)
            .map_err(Error::Bus)?;
        delay.delay_ms(RESET_MS);
        for (register, value) in [
            (POWER_MANAGEMENT_1, AUTO_CLOCK),
            (ACCEL_CONFIG, (accel as u8) << 3),
            (GYRO_CONFIG, (gyro as u8) << 3),
            (CONFIG, LOW_PASS_176_HZ),
            (SAMPLE_RATE_DIVIDER, divider),
            (INT_PIN_CONFIG, INTERRUPT_ACTIVE_LOW_OPEN_DRAIN),
            (INT_ENABLE, 0x00),
            (ACCEL_CONFIG_2, 0x00),
            (USER_CONTROL, 0x00),
            (FIFO_ENABLE, 0x00),
        ] {
            self.write_verified(i2c, register, value)?;
        }
        Ok(variant)
    }

    /// Reads the accelerometer, temperature, and gyroscope words in one
    /// transfer.
    ///
    /// # Errors
    ///
    /// Returns the bus error.
    pub fn read_raw<I2C: I2c>(&self, i2c: &mut I2C) -> Result<RawSample, I2C::Error> {
        let mut data = [0; 14];
        i2c.write_read(self.address, &[ACCEL_XOUT_H], &mut data)?;
        let word = |index: usize| i16::from_be_bytes([data[index], data[index | 1]]);
        Ok(RawSample {
            acceleration: [word(0), word(2), word(4)],
            temperature: word(6),
            angular_velocity: [word(8), word(10), word(12)],
        })
    }

    fn write_verified<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        register: u8,
        value: u8,
    ) -> Result<(), Error<I2C::Error>> {
        for _ in 0..WRITE_ATTEMPTS {
            self.write_register(i2c, register, value)
                .map_err(Error::Bus)?;
            if self.read_register(i2c, register).map_err(Error::Bus)? == value {
                return Ok(());
            }
        }
        Err(Error::WriteNotApplied(register))
    }

    fn read_register<I2C: I2c>(&self, i2c: &mut I2C, register: u8) -> Result<u8, I2C::Error> {
        let mut value = [0];
        i2c.write_read(self.address, &[register], &mut value)?;
        Ok(value[0])
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{AccelRange, DEFAULT_ADDRESS, Error, GyroRange, Mpu6886, Variant};

    struct Sensor {
        registers: [u8; 0x80],
        writes: Vec<(u8, u8)>,
        /// Registers that ignore this many writes before taking one.
        stubborn: Option<(u8, u8)>,
    }

    impl Sensor {
        fn new(who_am_i: u8) -> Self {
            let mut registers = [0; 0x80];
            registers[0x75] = who_am_i;
            Self {
                registers,
                writes: Vec::new(),
                stubborn: None,
            }
        }
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
            let mut pointer = 0;
            for operation in operations {
                match operation {
                    Operation::Write([register]) => pointer = usize::from(*register),
                    Operation::Write([register, value]) => {
                        self.writes.push((*register, *value));
                        match &mut self.stubborn {
                            Some((stubborn, remaining))
                                if stubborn == register && *remaining > 0 =>
                            {
                                *remaining -= 1;
                            }
                            _ => self.registers[usize::from(*register)] = *value,
                        }
                    }
                    Operation::Read(bytes) => {
                        for (offset, value) in bytes.iter_mut().enumerate() {
                            *value = self.registers[pointer + offset];
                        }
                    }
                    Operation::Write(_) => {}
                }
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay(u32);

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.0 += ns / 1_000_000;
        }
    }

    fn sensor() -> Mpu6886 {
        Mpu6886::new(DEFAULT_ADDRESS).expect("valid address")
    }

    #[test]
    fn initializes_with_m5unified_sequence_and_verifies_each_write() {
        let mut bus = Sensor::new(0x19);
        bus.stubborn = Some((0x1c, 2));
        let mut delay = Delay::default();
        let variant = sensor()
            .initialize(&mut bus, &mut delay, AccelRange::G8, GyroRange::Dps2000, 3)
            .expect("initialize");
        assert_eq!(variant, Variant::Mpu6886);
        assert_eq!(delay.0, 100);
        assert_eq!(bus.writes[0], (0x6b, 0x80));
        assert_eq!(bus.registers[0x6b], 0x01);
        assert_eq!(bus.registers[0x1c], 0x10);
        assert_eq!(bus.registers[0x1b], 0x18);
        assert_eq!(bus.registers[0x1a], 0x01);
        assert_eq!(bus.registers[0x19], 0x03);
        assert_eq!(bus.registers[0x37], 0xc0);
        let accel_writes = bus.writes.iter().filter(|(r, _)| *r == 0x1c).count();
        assert_eq!(accel_writes, 3);
    }

    #[test]
    fn rejects_another_chip_and_a_register_that_never_takes_its_value() {
        let mut bus = Sensor::new(0x24);
        assert!(matches!(
            sensor().initialize(
                &mut bus,
                &mut Delay::default(),
                AccelRange::G4,
                GyroRange::Dps500,
                9
            ),
            Err(Error::UnknownDevice(0x24))
        ));
        assert_eq!(sensor().identify(&mut bus).expect("read"), None);
        let mut bus = Sensor::new(0x71);
        bus.stubborn = Some((0x1b, 100));
        assert!(matches!(
            sensor().initialize(
                &mut bus,
                &mut Delay::default(),
                AccelRange::G4,
                GyroRange::Dps500,
                9
            ),
            Err(Error::WriteNotApplied(0x1b))
        ));
    }

    #[test]
    fn reads_big_endian_words_and_converts_each_parts_temperature() {
        let mut bus = Sensor::new(0x19);
        bus.registers[0x3b..0x49].copy_from_slice(&[
            0x10, 0x00, 0xf0, 0x00, 0x00, 0x01, 0x05, 0x1b, 0x00, 0x83, 0xff, 0x7d, 0x80, 0x00,
        ]);
        let raw = sensor().read_raw(&mut bus).expect("read");
        assert_eq!(raw.acceleration, [4_096, -4_096, 1]);
        assert_eq!(raw.temperature, 1_307);
        assert_eq!(raw.angular_velocity, [131, -131, i16::MIN]);
        // 1307 / 326.8 + 25 = 29.0 °C.
        assert_eq!(Variant::Mpu6886.temperature_millicelsius(1_307), 28_999);
        assert_eq!(Variant::Mpu9250.temperature_millicelsius(0), 21_000);
        assert_eq!(Variant::Mpu6050.temperature_millicelsius(-340), 35_530);
    }
}

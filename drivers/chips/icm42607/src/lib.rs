//! Register-level driver for the TDK InvenSense ICM-42607-P six-axis sensor
//! and the ICM-42670-P that shares its user register bank.
//!
//! Registers follow Espressif's `icm42670` component (esp-bsp
//! `components/sensors/icm42670`), which the ESP32-S3-BOX-3 BSP uses for its
//! ICM-42607-P, and the ICM-42670-P datasheet (DS-000451) bank 0 map:
//! `WHO_AM_I` (`75h`) reads `60h` on an ICM-42607-P and `67h` on an
//! ICM-42670-P; `GYRO_CONFIG0` (`20h`) and `ACCEL_CONFIG0` (`21h`) hold the
//! full scale in bits 6:5 and the output data rate in bits 3:0; `PWR_MGMT0`
//! (`1Fh`) switches both sensors to low-noise mode; and the big-endian
//! temperature, accelerometer, and gyroscope words run from `09h` to `16h`.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// The address with AD0 low (`ICM42670_I2C_ADDRESS`).
pub const DEFAULT_ADDRESS: u8 = 0x68;

const SIGNAL_PATH_RESET: u8 = 0x02;
const TEMP_DATA1: u8 = 0x09;
const POWER_MANAGEMENT_0: u8 = 0x1f;
/// `GYRO_CONFIG0`; `ACCEL_CONFIG0` follows it at `21h`.
const GYRO_CONFIG_0: u8 = 0x20;
const WHO_AM_I: u8 = 0x75;

/// `SIGNAL_PATH_RESET` SOFT_RESET_DEVICE_CONFIG: returns every register to
/// its reset value; the part takes 1 ms to restart.
const SOFT_RESET: u8 = 1 << 4;
const SOFT_RESET_MS: u32 = 2;
/// `PWR_MGMT0`: GYRO_MODE and ACCEL_MODE both low-noise (`11`).
const LOW_NOISE: u8 = 0b0000_1111;
/// The gyroscope needs 45 ms after it starts before its data is valid.
const GYRO_START_MS: u32 = 45;

/// Which part answered `WHO_AM_I`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// ICM-42607-P, `WHO_AM_I` `60h`.
    Icm42607,
    /// ICM-42670-P, `WHO_AM_I` `67h`.
    Icm42670,
}

impl Variant {
    /// Identifies a `WHO_AM_I` value.
    #[must_use]
    pub const fn from_who_am_i(value: u8) -> Option<Self> {
        match value {
            0x60 => Some(Self::Icm42607),
            0x67 => Some(Self::Icm42670),
            _ => None,
        }
    }
}

/// Accelerometer full scale, `ACCEL_UI_FS_SEL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccelRange {
    /// ±16 g.
    G16 = 0,
    /// ±8 g.
    G8 = 1,
    /// ±4 g.
    G4 = 2,
    /// ±2 g.
    G2 = 3,
}

/// Gyroscope full scale, `GYRO_UI_FS_SEL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GyroRange {
    /// ±2000 °/s.
    Dps2000 = 0,
    /// ±1000 °/s.
    Dps1000 = 1,
    /// ±500 °/s.
    Dps500 = 2,
    /// ±250 °/s.
    Dps250 = 3,
}

/// Low-noise output data rate shared by both sensors, `*_ODR`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataRate {
    /// 800 Hz.
    Hz800 = 6,
    /// 400 Hz.
    Hz400 = 7,
    /// 200 Hz.
    Hz200 = 8,
    /// 100 Hz.
    Hz100 = 9,
    /// 50 Hz.
    Hz50 = 10,
    /// 25 Hz.
    Hz25 = 11,
}

/// One raw sample in the sensor's own axes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawSample {
    /// Die temperature word: 128 LSB/°C, zero at 25 °C.
    pub temperature: i16,
    /// Accelerometer X, Y, Z.
    pub acceleration: [i16; 3],
    /// Gyroscope X, Y, Z.
    pub angular_velocity: [i16; 3],
}

impl RawSample {
    /// Converts the temperature word into milli-degrees Celsius.
    #[must_use]
    pub fn temperature_millicelsius(self) -> i32 {
        i32::from(self.temperature)
            .saturating_mul(1_000)
            .checked_div(128)
            .unwrap_or_default()
            .saturating_add(25_000)
    }
}

/// ICM-42607 transport or identity failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// `WHO_AM_I` named no supported part.
    UnknownDevice(u8),
}

/// An ICM-42607-P or ICM-42670-P at a caller-selected seven-bit address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Icm42607 {
    address: u8,
}

impl Icm42607 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Resets the part, sets both full scales and the data rate, and starts
    /// both sensors in low-noise mode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownDevice`] when another chip answers, or a bus
    /// error.
    pub fn initialize<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
        accel: AccelRange,
        gyro: GyroRange,
        rate: DataRate,
    ) -> Result<Variant, Error<I2C::Error>> {
        let mut who_am_i = [0];
        i2c.write_read(self.address, &[WHO_AM_I], &mut who_am_i)
            .map_err(Error::Bus)?;
        let variant =
            Variant::from_who_am_i(who_am_i[0]).ok_or(Error::UnknownDevice(who_am_i[0]))?;
        self.write(i2c, &[SIGNAL_PATH_RESET, SOFT_RESET])?;
        delay.delay_ms(SOFT_RESET_MS);
        // GYRO_CONFIG0 and ACCEL_CONFIG0 are adjacent; the icm42670
        // component writes them in one burst.
        self.write(
            i2c,
            &[
                GYRO_CONFIG_0,
                ((gyro as u8) << 5) | rate as u8,
                ((accel as u8) << 5) | rate as u8,
            ],
        )?;
        self.write(i2c, &[POWER_MANAGEMENT_0, LOW_NOISE])?;
        delay.delay_ms(GYRO_START_MS);
        Ok(variant)
    }

    /// Reads the temperature, accelerometer, and gyroscope words in one
    /// transfer.
    ///
    /// # Errors
    ///
    /// Returns the bus error.
    pub fn read_raw<I2C: I2c>(&self, i2c: &mut I2C) -> Result<RawSample, I2C::Error> {
        let mut data = [0; 14];
        i2c.write_read(self.address, &[TEMP_DATA1], &mut data)?;
        let word = |index: usize| i16::from_be_bytes([data[index], data[index | 1]]);
        Ok(RawSample {
            temperature: word(0),
            acceleration: [word(2), word(4), word(6)],
            angular_velocity: [word(8), word(10), word(12)],
        })
    }

    fn write<I2C: I2c>(&self, i2c: &mut I2C, bytes: &[u8]) -> Result<(), Error<I2C::Error>> {
        i2c.write(self.address, bytes).map_err(Error::Bus)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::{vec, vec::Vec};

    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{AccelRange, DEFAULT_ADDRESS, DataRate, Error, GyroRange, Icm42607, Variant};

    struct Sensor {
        registers: [u8; 0x80],
        writes: Vec<Vec<u8>>,
    }

    impl ErrorType for Sensor {
        type Error = Infallible;
    }

    impl I2c for Sensor {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut pointer = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        pointer = usize::from(bytes[0]);
                        if bytes.len() > 1 {
                            self.writes.push(bytes.to_vec());
                        }
                    }
                    Operation::Read(bytes) => {
                        for (offset, value) in bytes.iter_mut().enumerate() {
                            *value = self.registers[pointer + offset];
                        }
                    }
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

    fn sensor(who_am_i: u8) -> Sensor {
        let mut registers = [0; 0x80];
        registers[0x75] = who_am_i;
        Sensor {
            registers,
            writes: Vec::new(),
        }
    }

    #[test]
    fn resets_configures_and_starts_both_sensors() {
        let mut bus = sensor(0x60);
        let mut delay = Delay::default();
        let chip = Icm42607::new(DEFAULT_ADDRESS).expect("valid address");
        let variant = chip
            .initialize(
                &mut bus,
                &mut delay,
                AccelRange::G4,
                GyroRange::Dps500,
                DataRate::Hz100,
            )
            .expect("initialize");
        assert_eq!(variant, Variant::Icm42607);
        assert_eq!(
            bus.writes,
            [vec![0x02, 0x10], vec![0x20, 0x49, 0x49], vec![0x1f, 0x0f]]
        );
        assert_eq!(delay.0, 47);
    }

    #[test]
    fn rejects_another_chip() {
        let mut bus = sensor(0x19);
        let chip = Icm42607::new(DEFAULT_ADDRESS).expect("valid address");
        assert!(matches!(
            chip.initialize(
                &mut bus,
                &mut Delay::default(),
                AccelRange::G4,
                GyroRange::Dps500,
                DataRate::Hz100,
            ),
            Err(Error::UnknownDevice(0x19))
        ));
    }

    #[test]
    fn reads_temperature_then_accelerometer_then_gyroscope() {
        let mut bus = sensor(0x67);
        bus.registers[0x09..0x17].copy_from_slice(&[
            0x02, 0x80, 0x20, 0x00, 0xe0, 0x00, 0x00, 0x00, 0x00, 0x83, 0xff, 0x7d, 0x00, 0x00,
        ]);
        let chip = Icm42607::new(DEFAULT_ADDRESS).expect("valid address");
        let raw = chip.read_raw(&mut bus).expect("read");
        assert_eq!(raw.temperature, 640);
        assert_eq!(raw.temperature_millicelsius(), 30_000);
        assert_eq!(raw.acceleration, [8_192, -8_192, 0]);
        assert_eq!(raw.angular_velocity, [131, -131, 0]);
    }
}

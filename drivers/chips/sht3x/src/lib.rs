//! Command-level driver for the Sensirion SHT30, SHT31, and SHT35 humidity
//! and temperature sensors.
//!
//! Commands, timings, and conversions follow the SHT3x-DIS datasheet and
//! Sensirion's `embedded-sht` reference driver (`sht3x.c`): a single-shot high-repeatability measurement without
//! clock stretching (`2400h`) takes at most 15.5 ms; every 16-bit word the
//! sensor returns is followed by a CRC-8 (polynomial 31h, initial value FFh).
//!
//! The upstream `sht3x` and `sht31` crates are not used: they own the bus,
//! so one implementation cannot drive this sensor beside another chip on
//! the same I2C device view, and they convert through floating point.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// The address with the ADDR pin low; `45h` with it high.
pub const DEFAULT_ADDRESS: u8 = 0x44;

/// Single shot, high repeatability, clock stretching disabled.
const MEASURE_HIGH_REPEATABILITY: [u8; 2] = [0x24, 0x00];
const SOFT_RESET: [u8; 2] = [0x30, 0xa2];
const READ_STATUS: [u8; 2] = [0xf3, 0x2d];
/// High-repeatability measurement duration, 15.5 ms at most (datasheet;
/// `SHT3X_MEASUREMENT_DURATION_USEC` in `embedded-sht`).
const MEASUREMENT_MS: u32 = 16;
/// Soft reset time, 1.5 ms at most (datasheet).
const SOFT_RESET_MS: u32 = 2;
/// The wait `embedded-sht` leaves between a command and its read
/// (`SHT3X_CMD_DURATION_USEC`).
const COMMAND_SPACING_US: u32 = 1_000;

/// One temperature and humidity measurement in integer SI subunits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measurement {
    /// Temperature in milli-degrees Celsius.
    pub temperature_millicelsius: i32,
    /// Relative humidity in thousandths of a percent.
    pub relative_humidity_millipercent: u32,
}

/// SHT3x transport or data failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// A returned word did not match its CRC.
    Crc,
}

/// An SHT3x at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sht3x {
    address: u8,
}

impl Sht3x {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Resets the sensor's state and waits until it accepts commands.
    ///
    /// # Errors
    ///
    /// Returns the bus error of the reset command.
    pub fn soft_reset<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<(), I2C::Error> {
        i2c.write(self.address, &SOFT_RESET)?;
        delay.delay_ms(SOFT_RESET_MS);
        Ok(())
    }

    /// Reads the status register, which proves an SHT3x answers.
    ///
    /// # Errors
    ///
    /// Returns a bus error or [`Error::Crc`].
    pub fn read_status<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<u16, Error<I2C::Error>> {
        i2c.write(self.address, &READ_STATUS).map_err(Error::Bus)?;
        delay.delay_us(COMMAND_SPACING_US);
        let mut data = [0; 3];
        i2c.read(self.address, &mut data).map_err(Error::Bus)?;
        word(data).ok_or(Error::Crc)
    }

    /// Takes one high-repeatability measurement, waiting for it to finish.
    ///
    /// # Errors
    ///
    /// Returns a bus error or [`Error::Crc`].
    pub fn measure<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<Measurement, Error<I2C::Error>> {
        i2c.write(self.address, &MEASURE_HIGH_REPEATABILITY)
            .map_err(Error::Bus)?;
        delay.delay_ms(MEASUREMENT_MS);
        let mut data = [0; 6];
        i2c.read(self.address, &mut data).map_err(Error::Bus)?;
        let temperature = word([data[0], data[1], data[2]]).ok_or(Error::Crc)?;
        let humidity = word([data[3], data[4], data[5]]).ok_or(Error::Crc)?;
        Ok(Measurement {
            temperature_millicelsius: temperature_millicelsius(temperature),
            relative_humidity_millipercent: humidity_millipercent(humidity),
        })
    }
}

/// The datasheet's conversion: T = -45 + 175 * S / (2^16 - 1) degrees Celsius.
#[must_use]
pub fn temperature_millicelsius(raw: u16) -> i32 {
    let scaled = u64::from(raw).saturating_mul(175_000) / 65_535;
    i32::try_from(scaled)
        .unwrap_or(i32::MAX)
        .saturating_sub(45_000)
}

/// The datasheet's conversion: RH = 100 * S / (2^16 - 1) percent.
#[must_use]
pub fn humidity_millipercent(raw: u16) -> u32 {
    u32::try_from(u64::from(raw).saturating_mul(100_000) / 65_535).unwrap_or(u32::MAX)
}

/// Checks one big-endian word against the CRC byte that follows it.
fn word([high, low, crc]: [u8; 3]) -> Option<u16> {
    (crc8(&[high, low]) == crc).then_some(u16::from_be_bytes([high, low]))
}

/// Sensirion CRC-8: polynomial 31h, initial value FFh, no final XOR.
#[must_use]
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0xff_u8;
    for byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 == 0 {
                crc.wrapping_shl(1)
            } else {
                crc.wrapping_shl(1) ^ 0x31
            };
        }
    }
    crc
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

    use super::{DEFAULT_ADDRESS, Error, Measurement, Sht3x, crc8};

    /// Answers each command with the words the datasheet example gives.
    struct Sensor {
        last: Vec<u8>,
        commands: Vec<Vec<u8>>,
        temperature: u16,
        humidity: u16,
        corrupt: bool,
    }

    impl Sensor {
        fn new(temperature: u16, humidity: u16) -> Self {
            Self {
                last: Vec::new(),
                commands: Vec::new(),
                temperature,
                humidity,
                corrupt: false,
            }
        }
    }

    fn framed(words: &[u16]) -> Vec<u8> {
        words
            .iter()
            .flat_map(|word| {
                let [high, low] = word.to_be_bytes();
                [high, low, crc8(&[high, low])]
            })
            .collect()
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
                        self.last = bytes.to_vec();
                        self.commands.push(bytes.to_vec());
                    }
                    Operation::Read(buffer) => {
                        let mut reply = match self.last.as_slice() {
                            [0x24, 0x00] => framed(&[self.temperature, self.humidity]),
                            [0xf3, 0x2d] => framed(&[0x8010]),
                            _ => vec![0xff; buffer.len()],
                        };
                        if self.corrupt {
                            reply[2] ^= 1;
                        }
                        buffer.copy_from_slice(&reply[..buffer.len()]);
                    }
                }
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay(u64);

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.0 += u64::from(ns);
        }
    }

    fn sensor() -> Sht3x {
        Sht3x::new(DEFAULT_ADDRESS).expect("valid address")
    }

    #[test]
    fn crc_matches_the_datasheet_example() {
        // The Sensirion datasheets' CRC example: CRC(BEEFh) = 92h.
        assert_eq!(crc8(&[0xbe, 0xef]), 0x92);
    }

    #[test]
    fn measures_and_converts_the_full_scale() {
        let mut bus = Sensor::new(0x6666, 0x8000);
        let mut delay = Delay::default();
        sensor().soft_reset(&mut bus, &mut delay).expect("reset");
        assert_eq!(
            sensor().read_status(&mut bus, &mut delay).expect("status"),
            0x8010
        );
        let measurement = sensor().measure(&mut bus, &mut delay).expect("measure");
        // 0x6666 is 0.4 of full scale: -45 + 70 = 25 degrees Celsius.
        assert_eq!(
            measurement,
            Measurement {
                temperature_millicelsius: 25_000,
                relative_humidity_millipercent: 50_000,
            }
        );
        assert_eq!(
            bus.commands,
            [vec![0x30, 0xa2], vec![0xf3, 0x2d], vec![0x24, 0x00]]
        );
        // The measurement waited for the conversion to finish.
        assert!(delay.0 >= 16_000_000);
    }

    #[test]
    fn converts_the_range_ends() {
        assert_eq!(super::temperature_millicelsius(0), -45_000);
        assert_eq!(super::temperature_millicelsius(u16::MAX), 130_000);
        assert_eq!(super::humidity_millipercent(u16::MAX), 100_000);
    }

    #[test]
    fn rejects_a_corrupt_word_and_a_wide_address() {
        let mut bus = Sensor::new(0, 0);
        bus.corrupt = true;
        assert!(matches!(
            sensor().measure(&mut bus, &mut Delay::default()),
            Err(Error::Crc)
        ));
        assert!(Sht3x::new(0x80).is_none());
    }
}

//! Command-level driver for the Sensirion SHT40, SHT41, SHT43, and SHT45
//! humidity and temperature sensors.
//!
//! Commands, timings, and conversions follow the SHT4x datasheet and
//! Sensirion's `embedded-i2c-sht4x` reference driver (`sht4x_i2c.c`): the
//! one-byte commands `FDh` (high-precision measurement, 8.3 ms at most;
//! the reference waits 10 ms), `89h` (serial number), and `94h` (soft
//! reset). Every 16-bit word the sensor returns is followed by a CRC-8
//! (polynomial 31h, initial value FFh).
//!
//! The upstream `sht4x` crate is not used: it owns the bus, so one
//! implementation cannot drive this sensor beside another chip on the same
//! I2C device view.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// The SHT40-AD1B address; other variants answer at `45h` or `46h`.
pub const DEFAULT_ADDRESS: u8 = 0x44;

const MEASURE_HIGH_PRECISION: u8 = 0xfd;
const READ_SERIAL: u8 = 0x89;
const SOFT_RESET: u8 = 0x94;
/// `sht4x_measure_high_precision_ticks` waits 10 ms.
const MEASUREMENT_MS: u32 = 10;
/// `sht4x_serial_number` and `sht4x_soft_reset` wait 10 ms.
const COMMAND_MS: u32 = 10;

/// One temperature and humidity measurement in integer SI subunits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measurement {
    /// Temperature in milli-degrees Celsius.
    pub temperature_millicelsius: i32,
    /// Relative humidity in thousandths of a percent, clamped to 0 through
    /// 100 000 as the datasheet advises.
    pub relative_humidity_millipercent: u32,
}

/// SHT4x transport or data failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// A returned word did not match its CRC.
    Crc,
}

/// An SHT4x at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sht4x {
    address: u8,
}

impl Sht4x {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Resets the sensor and waits until it accepts commands.
    ///
    /// # Errors
    ///
    /// Returns the bus error of the reset command.
    pub fn soft_reset<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<(), I2C::Error> {
        i2c.write(self.address, &[SOFT_RESET])?;
        delay.delay_ms(COMMAND_MS);
        Ok(())
    }

    /// Reads the 32-bit serial number, which proves an SHT4x answers.
    ///
    /// # Errors
    ///
    /// Returns a bus error or [`Error::Crc`].
    pub fn serial_number<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<u32, Error<I2C::Error>> {
        let [high, low] = self.command(i2c, delay, READ_SERIAL, COMMAND_MS)?;
        Ok(u32::from(high) << 16 | u32::from(low))
    }

    /// Takes one high-precision measurement, waiting for it to finish.
    ///
    /// # Errors
    ///
    /// Returns a bus error or [`Error::Crc`].
    pub fn measure<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<Measurement, Error<I2C::Error>> {
        let [temperature, humidity] =
            self.command(i2c, delay, MEASURE_HIGH_PRECISION, MEASUREMENT_MS)?;
        Ok(Measurement {
            temperature_millicelsius: temperature_millicelsius(temperature),
            relative_humidity_millipercent: humidity_millipercent(humidity),
        })
    }

    fn command<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
        command: u8,
        wait_ms: u32,
    ) -> Result<[u16; 2], Error<I2C::Error>> {
        i2c.write(self.address, &[command]).map_err(Error::Bus)?;
        delay.delay_ms(wait_ms);
        let mut data = [0; 6];
        i2c.read(self.address, &mut data).map_err(Error::Bus)?;
        Ok([
            word([data[0], data[1], data[2]]).ok_or(Error::Crc)?,
            word([data[3], data[4], data[5]]).ok_or(Error::Crc)?,
        ])
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

/// The datasheet's conversion: RH = -6 + 125 * S / (2^16 - 1) percent,
/// clamped to 0 through 100 percent.
#[must_use]
pub fn humidity_millipercent(raw: u16) -> u32 {
    let scaled = u64::from(raw).saturating_mul(125_000) / 65_535;
    u32::try_from(scaled.saturating_sub(6_000).min(100_000)).unwrap_or(100_000)
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

    use super::{DEFAULT_ADDRESS, Error, Measurement, Sht4x, crc8};

    struct Sensor {
        last: u8,
        commands: Vec<u8>,
        words: [u16; 2],
        corrupt: bool,
    }

    impl Sensor {
        fn new(words: [u16; 2]) -> Self {
            Self {
                last: 0,
                commands: Vec::new(),
                words,
                corrupt: false,
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
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        assert_eq!(bytes.len(), 1, "one-byte commands");
                        self.last = bytes[0];
                        self.commands.push(bytes[0]);
                    }
                    Operation::Read(buffer) => {
                        let words = match self.last {
                            0xfd => self.words,
                            0x89 => [0x0123, 0x4567],
                            _ => [0xffff, 0xffff],
                        };
                        let mut reply: Vec<u8> = words
                            .iter()
                            .flat_map(|word| {
                                let [high, low] = word.to_be_bytes();
                                [high, low, crc8(&[high, low])]
                            })
                            .collect();
                        if self.corrupt {
                            reply[5] ^= 1;
                        }
                        buffer.copy_from_slice(&reply);
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

    fn sensor() -> Sht4x {
        Sht4x::new(DEFAULT_ADDRESS).expect("valid address")
    }

    #[test]
    fn resets_identifies_and_measures() {
        let mut bus = Sensor::new([0x6666, 0x8000]);
        let mut delay = Delay::default();
        sensor().soft_reset(&mut bus, &mut delay).expect("reset");
        assert_eq!(
            sensor()
                .serial_number(&mut bus, &mut delay)
                .expect("serial"),
            0x0123_4567
        );
        assert_eq!(
            sensor().measure(&mut bus, &mut delay).expect("measure"),
            Measurement {
                temperature_millicelsius: 25_000,
                // -6 + 125 * 0.50000763 = 56.5 percent.
                relative_humidity_millipercent: 56_500,
            }
        );
        assert_eq!(bus.commands, vec![0x94, 0x89, 0xfd]);
        assert!(delay.0 >= 30_000_000);
    }

    #[test]
    fn clamps_humidity_to_its_physical_range() {
        assert_eq!(super::humidity_millipercent(0), 0);
        assert_eq!(super::humidity_millipercent(u16::MAX), 100_000);
        assert_eq!(super::temperature_millicelsius(0), -45_000);
    }

    #[test]
    fn rejects_a_corrupt_word() {
        let mut bus = Sensor::new([0, 0]);
        bus.corrupt = true;
        assert!(matches!(
            sensor().measure(&mut bus, &mut Delay::default()),
            Err(Error::Crc)
        ));
        assert!(Sht4x::new(0xff).is_none());
    }
}

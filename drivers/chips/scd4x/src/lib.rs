//! Command-level driver for the Sensirion SCD40 and SCD41 CO2, temperature,
//! and humidity sensors.
//!
//! Commands, timings, and conversions follow the SCD4x datasheet and
//! Sensirion's `embedded-i2c-scd4x` reference driver (`scd4x_i2c.c`):
//! `21B1h` starts periodic measurement (one sample every five seconds),
//! `3F86h` stops it (500 ms), `E4B8h` reports whether a sample is ready
//! (its low eleven bits are non-zero), `EC05h` reads the sample, and
//! `3682h` reads the 48-bit serial number; the reads wait 1 ms between the
//! command and the data. Every 16-bit word is followed by a CRC-8
//! (polynomial 31h, initial value FFh).
//!
//! The upstream `scd4x` crate is not used: it owns the bus and a delay, so
//! one implementation cannot drive this sensor beside another chip on the
//! same I2C device view, and it converts through floating point.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// The SCD4x's fixed address.
pub const ADDRESS: u8 = 0x62;

const START_PERIODIC_MEASUREMENT: u16 = 0x21b1;
const STOP_PERIODIC_MEASUREMENT: u16 = 0x3f86;
const GET_DATA_READY_STATUS: u16 = 0xe4b8;
const READ_MEASUREMENT: u16 = 0xec05;
const GET_SERIAL_NUMBER: u16 = 0x3682;
/// `scd4x_stop_periodic_measurement` waits 500 ms before the sensor
/// accepts another command.
const STOP_MS: u32 = 500;
/// The wait between a read command and its data.
const READ_MS: u32 = 1;

/// One CO2, temperature, and humidity sample in integer SI subunits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measurement {
    /// CO2 concentration in parts per million.
    pub co2_ppm: u16,
    /// Temperature in milli-degrees Celsius.
    pub temperature_millicelsius: i32,
    /// Relative humidity in thousandths of a percent.
    pub relative_humidity_millipercent: u32,
}

/// SCD4x transport or data failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// A returned word did not match its CRC.
    Crc,
}

/// An SCD4x at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scd4x {
    address: u8,
}

impl Scd4x {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Stops periodic measurement and waits until the sensor is idle. The
    /// sensor keeps measuring across a host reset, and accepts most
    /// commands only while idle, so a host stops it before configuring it.
    ///
    /// # Errors
    ///
    /// Returns the bus error of the command.
    pub fn stop_periodic_measurement<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<(), I2C::Error> {
        self.send(i2c, STOP_PERIODIC_MEASUREMENT)?;
        delay.delay_ms(STOP_MS);
        Ok(())
    }

    /// Reads the 48-bit serial number, which proves an SCD4x answers. The
    /// sensor must be idle.
    ///
    /// # Errors
    ///
    /// Returns a bus error or [`Error::Crc`].
    pub fn serial_number<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<u64, Error<I2C::Error>> {
        let [high, middle, low] = self.read_words(i2c, delay, GET_SERIAL_NUMBER)?;
        Ok(u64::from(high) << 32 | u64::from(middle) << 16 | u64::from(low))
    }

    /// Starts periodic measurement; the first sample is ready after five
    /// seconds.
    ///
    /// # Errors
    ///
    /// Returns the bus error of the command.
    pub fn start_periodic_measurement<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), I2C::Error> {
        self.send(i2c, START_PERIODIC_MEASUREMENT)
    }

    /// Reports whether a new sample is ready to read.
    ///
    /// # Errors
    ///
    /// Returns a bus error or [`Error::Crc`].
    pub fn data_ready<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<bool, Error<I2C::Error>> {
        let [status] = self.read_words(i2c, delay, GET_DATA_READY_STATUS)?;
        Ok(status & 0x07ff != 0)
    }

    /// Reads the latest sample, which clears the ready status.
    ///
    /// # Errors
    ///
    /// Returns a bus error or [`Error::Crc`].
    pub fn read_measurement<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<Measurement, Error<I2C::Error>> {
        let [co2, temperature, humidity] = self.read_words(i2c, delay, READ_MEASUREMENT)?;
        Ok(Measurement {
            co2_ppm: co2,
            temperature_millicelsius: temperature_millicelsius(temperature),
            relative_humidity_millipercent: humidity_millipercent(humidity),
        })
    }

    fn send<I2C: I2c>(&self, i2c: &mut I2C, command: u16) -> Result<(), I2C::Error> {
        i2c.write(self.address, &command.to_be_bytes())
    }

    fn read_words<I2C: I2c, D: DelayNs, const N: usize>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
        command: u16,
    ) -> Result<[u16; N], Error<I2C::Error>> {
        self.send(i2c, command).map_err(Error::Bus)?;
        delay.delay_ms(READ_MS);
        // Three words at most, each with its CRC.
        let mut data = [0; 9];
        let length = N.saturating_mul(3).min(data.len());
        let data = data.get_mut(..length).ok_or(Error::Crc)?;
        i2c.read(self.address, data).map_err(Error::Bus)?;
        let mut words = [0; N];
        for (word, chunk) in words.iter_mut().zip(data.as_chunks::<3>().0) {
            *word = checked_word(*chunk).ok_or(Error::Crc)?;
        }
        Ok(words)
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
fn checked_word([high, low, crc]: [u8; 3]) -> Option<u16> {
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

    use super::{ADDRESS, Measurement, Scd4x, crc8};

    /// A sensor that has a sample once periodic measurement has started.
    struct Sensor {
        last: u16,
        commands: Vec<u16>,
        measuring: bool,
        ready: bool,
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
            assert_eq!(address, ADDRESS);
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        let command = u16::from_be_bytes([bytes[0], bytes[1]]);
                        self.last = command;
                        self.commands.push(command);
                        match command {
                            0x21b1 => self.measuring = true,
                            0x3f86 => self.measuring = false,
                            _ => {}
                        }
                    }
                    Operation::Read(buffer) => {
                        let words: Vec<u16> = match self.last {
                            0x3682 => vec![0xf896, 0x9f07, 0x3bb5],
                            0xe4b8 => vec![if self.ready { 0x8006 } else { 0x8000 }],
                            // Datasheet example: 500 ppm, 25 degrees Celsius
                            // (6667h), 37 percent (5EB9h).
                            0xec05 => {
                                self.ready = false;
                                vec![0x01f4, 0x6667, 0x5eb9]
                            }
                            _ => vec![0xffff; buffer.len() / 3],
                        };
                        let reply: Vec<u8> = words
                            .iter()
                            .flat_map(|word| {
                                let [high, low] = word.to_be_bytes();
                                [high, low, crc8(&[high, low])]
                            })
                            .collect();
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

    fn sensor() -> Scd4x {
        Scd4x::new(ADDRESS).expect("valid address")
    }

    #[test]
    fn stops_identifies_starts_and_reads_a_sample() {
        let mut bus = Sensor {
            last: 0,
            commands: Vec::new(),
            measuring: true,
            ready: false,
        };
        let mut delay = Delay::default();
        sensor()
            .stop_periodic_measurement(&mut bus, &mut delay)
            .expect("stop");
        assert!(!bus.measuring);
        assert!(delay.0 >= 500_000_000);
        assert_eq!(
            sensor()
                .serial_number(&mut bus, &mut delay)
                .expect("serial"),
            0xf896_9f07_3bb5
        );
        sensor()
            .start_periodic_measurement(&mut bus)
            .expect("start");
        assert!(!sensor().data_ready(&mut bus, &mut delay).expect("status"));
        bus.ready = true;
        assert!(sensor().data_ready(&mut bus, &mut delay).expect("status"));
        assert_eq!(
            sensor()
                .read_measurement(&mut bus, &mut delay)
                .expect("sample"),
            Measurement {
                co2_ppm: 500,
                temperature_millicelsius: 25_002,
                relative_humidity_millipercent: 37_001,
            }
        );
        assert_eq!(
            bus.commands,
            [0x3f86, 0x3682, 0x21b1, 0xe4b8, 0xe4b8, 0xec05]
        );
    }

    #[test]
    fn rejects_a_corrupt_word() {
        assert_eq!(super::checked_word([0xbe, 0xef, 0x92]), Some(0xbeef));
        assert_eq!(super::checked_word([0xbe, 0xef, 0x93]), None);
        assert!(Scd4x::new(0x80).is_none());
    }
}

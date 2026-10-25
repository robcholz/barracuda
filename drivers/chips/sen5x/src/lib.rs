//! Command-level driver for the Sensirion SEN50, SEN54, and SEN55
//! environmental sensor nodes.
//!
//! Commands, timings, and scaling follow the SEN5x datasheet and Sensirion's
//! `embedded-i2c-sen5x` reference driver (`sen5x_i2c.c`):
//!
//! | Command | Code | Wait |
//! | --- | --- | --- |
//! | Device reset | `D304h` | 200 ms |
//! | Read product name | `D014h` | 50 ms |
//! | Start measurement | `0021h` | 50 ms |
//! | Read measured values | `03C4h` | 20 ms |
//!
//! Measured values are eight words: PM1.0, PM2.5, PM4.0, and PM10 in
//! tenths of µg/m³ (unsigned, `FFFFh` unknown); humidity in hundredths of a
//! percent, temperature in two-hundredths of a degree Celsius, and the VOC
//! and NOx indices in tenths (signed, `7FFFh` unknown). The sensor returns
//! its latest values when no new measurement is ready, and the unknown
//! marker until the first one (and, for NOx, during its warm-up). Every
//! 16-bit word is followed by a CRC-8 (polynomial 31h, initial value FFh).
//!
//! The upstream `sen5x` and `sen5x-rs` crates are not used: they own the
//! bus and a delay, so one implementation cannot drive this sensor beside
//! another chip on the same I2C device view.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// The SEN5x's fixed address.
pub const ADDRESS: u8 = 0x69;

const DEVICE_RESET: u16 = 0xd304;
const READ_PRODUCT_NAME: u16 = 0xd014;
const START_MEASUREMENT: u16 = 0x0021;
const READ_MEASURED_VALUES: u16 = 0x03c4;
const RESET_MS: u32 = 200;
const PRODUCT_NAME_MS: u32 = 50;
const START_MS: u32 = 50;
const READ_MS: u32 = 20;

/// The SEN5x variant, which decides what it measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// Particulate matter only.
    Sen50,
    /// Particulate matter, humidity, temperature, and VOC index.
    Sen54,
    /// Particulate matter, humidity, temperature, VOC and NOx indices.
    Sen55,
}

impl Model {
    /// Returns whether the variant measures humidity, temperature, and VOC.
    #[must_use]
    pub const fn has_gas_and_climate(self) -> bool {
        !matches!(self, Self::Sen50)
    }

    /// Returns whether the variant measures the NOx index.
    #[must_use]
    pub const fn has_nox(self) -> bool {
        matches!(self, Self::Sen55)
    }
}

/// One measurement in integer SI subunits; `None` where the sensor reports
/// no valid value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Measurement {
    /// PM1.0 mass concentration in nanograms per cubic metre.
    pub pm1_0_ng_per_m3: Option<u32>,
    /// PM2.5 mass concentration in nanograms per cubic metre.
    pub pm2_5_ng_per_m3: Option<u32>,
    /// PM4.0 mass concentration in nanograms per cubic metre.
    pub pm4_0_ng_per_m3: Option<u32>,
    /// PM10 mass concentration in nanograms per cubic metre.
    pub pm10_ng_per_m3: Option<u32>,
    /// Relative humidity in thousandths of a percent.
    pub relative_humidity_millipercent: Option<u32>,
    /// Temperature in milli-degrees Celsius.
    pub temperature_millicelsius: Option<i32>,
    /// VOC index in thousandths.
    pub voc_index_milli: Option<u32>,
    /// NOx index in thousandths.
    pub nox_index_milli: Option<u32>,
}

/// SEN5x transport or data failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// A returned word did not match its CRC.
    Crc,
    /// The product name is not SEN50, SEN54, or SEN55.
    UnknownProduct,
}

/// A SEN5x at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sen5x {
    address: u8,
}

impl Sen5x {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Resets the sensor to idle and waits until it accepts commands.
    ///
    /// # Errors
    ///
    /// Returns the bus error of the command.
    pub fn reset<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<(), I2C::Error> {
        i2c.write(self.address, &DEVICE_RESET.to_be_bytes())?;
        delay.delay_ms(RESET_MS);
        Ok(())
    }

    /// Reads the product name and identifies the variant.
    ///
    /// # Errors
    ///
    /// Returns a bus error, [`Error::Crc`], or [`Error::UnknownProduct`].
    pub fn model<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<Model, Error<I2C::Error>> {
        // The name is up to 32 ASCII bytes, NUL-padded; the variant is in
        // its first five.
        let mut name = [0; 6];
        let words: [u16; 3] = self.read_words(i2c, delay, READ_PRODUCT_NAME, PRODUCT_NAME_MS)?;
        for (pair, word) in name.as_chunks_mut::<2>().0.iter_mut().zip(words) {
            pair.copy_from_slice(&word.to_be_bytes());
        }
        match &name[..5] {
            b"SEN50" => Ok(Model::Sen50),
            b"SEN54" => Ok(Model::Sen54),
            b"SEN55" => Ok(Model::Sen55),
            _ => Err(Error::UnknownProduct),
        }
    }

    /// Starts continuous measurement; values arrive once per second.
    ///
    /// # Errors
    ///
    /// Returns the bus error of the command.
    pub fn start_measurement<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<(), I2C::Error> {
        i2c.write(self.address, &START_MEASUREMENT.to_be_bytes())?;
        delay.delay_ms(START_MS);
        Ok(())
    }

    /// Reads the latest measured values.
    ///
    /// # Errors
    ///
    /// Returns a bus error or [`Error::Crc`].
    pub fn read_measured_values<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<Measurement, Error<I2C::Error>> {
        let [pm1_0, pm2_5, pm4_0, pm10, humidity, temperature, voc, nox]: [u16; 8] =
            self.read_words(i2c, delay, READ_MEASURED_VALUES, READ_MS)?;
        Ok(Measurement {
            pm1_0_ng_per_m3: mass_concentration(pm1_0),
            pm2_5_ng_per_m3: mass_concentration(pm2_5),
            pm4_0_ng_per_m3: mass_concentration(pm4_0),
            pm10_ng_per_m3: mass_concentration(pm10),
            // Hundredths of a percent: ten thousandths of a percent each.
            relative_humidity_millipercent: signed(humidity)
                .and_then(|value| u32::try_from(i32::from(value).saturating_mul(10)).ok()),
            // Two-hundredths of a degree: five milli-degrees each.
            temperature_millicelsius: signed(temperature)
                .map(|value| i32::from(value).saturating_mul(5)),
            voc_index_milli: index(voc),
            nox_index_milli: index(nox),
        })
    }

    fn read_words<I2C: I2c, D: DelayNs, const N: usize>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
        command: u16,
        wait_ms: u32,
    ) -> Result<[u16; N], Error<I2C::Error>> {
        i2c.write(self.address, &command.to_be_bytes())
            .map_err(Error::Bus)?;
        delay.delay_ms(wait_ms);
        // Eight words at most, each with its CRC.
        let mut data = [0; 24];
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

/// Tenths of µg/m³ to ng/m³; `FFFFh` is unknown.
fn mass_concentration(raw: u16) -> Option<u32> {
    (raw != u16::MAX).then(|| u32::from(raw).saturating_mul(100))
}

/// A signed word; `7FFFh` is unknown.
fn signed(raw: u16) -> Option<i16> {
    let value = i16::from_be_bytes(raw.to_be_bytes());
    (value != i16::MAX).then_some(value)
}

/// Tenths of an index point to thousandths; the index is never below 1.
fn index(raw: u16) -> Option<u32> {
    signed(raw)
        .and_then(|value| u32::try_from(value).ok())
        .map(|value| value.saturating_mul(100))
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

    use super::{ADDRESS, Error, Measurement, Model, Sen5x, crc8};

    struct Sensor {
        name: &'static [u8],
        last: u16,
        commands: Vec<u16>,
        measuring: bool,
    }

    impl Sensor {
        fn new(name: &'static [u8]) -> Self {
            Self {
                name,
                last: 0,
                commands: Vec::new(),
                measuring: false,
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
            assert_eq!(address, ADDRESS);
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        let command = u16::from_be_bytes([bytes[0], bytes[1]]);
                        self.last = command;
                        self.commands.push(command);
                        if command == 0x0021 {
                            self.measuring = true;
                        }
                    }
                    Operation::Read(buffer) => {
                        let bytes: Vec<u8> = match self.last {
                            0xd014 => {
                                let mut name = self.name.to_vec();
                                name.resize(32, 0);
                                name
                            }
                            0x03c4 if self.measuring => {
                                [12_u16, 34, 45, 56, 4_512, 4_700, 1_005, 10]
                                    .iter()
                                    .flat_map(|word| word.to_be_bytes())
                                    .collect()
                            }
                            // Before the first measurement: all unknown.
                            0x03c4 => [
                                0xffff_u16, 0xffff, 0xffff, 0xffff, 0x7fff, 0x7fff, 0x7fff, 0x7fff,
                            ]
                            .iter()
                            .flat_map(|word| word.to_be_bytes())
                            .collect(),
                            _ => vec![0xff; buffer.len() / 3 * 2],
                        };
                        let reply: Vec<u8> = bytes
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .flat_map(|pair| [pair[0], pair[1], crc8(pair)])
                            .take(buffer.len())
                            .collect();
                        buffer.copy_from_slice(&reply[..buffer.len()]);
                    }
                }
            }
            Ok(())
        }
    }

    struct Delay;

    impl DelayNs for Delay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    fn sensor() -> Sen5x {
        Sen5x::new(ADDRESS).expect("valid address")
    }

    #[test]
    fn identifies_the_variant_from_its_product_name() {
        for (name, model) in [
            (&b"SEN50"[..], Model::Sen50),
            (b"SEN54", Model::Sen54),
            (b"SEN55", Model::Sen55),
        ] {
            let mut bus = Sensor::new(name);
            assert_eq!(sensor().model(&mut bus, &mut Delay).expect("model"), model);
        }
        let mut bus = Sensor::new(b"SCD41");
        assert!(matches!(
            sensor().model(&mut bus, &mut Delay),
            Err(Error::UnknownProduct)
        ));
        assert!(Model::Sen55.has_nox() && !Model::Sen54.has_nox());
        assert!(!Model::Sen50.has_gas_and_climate());
    }

    #[test]
    fn reports_unknown_values_until_the_first_measurement() {
        let mut bus = Sensor::new(b"SEN55");
        sensor().reset(&mut bus, &mut Delay).expect("reset");
        assert_eq!(
            sensor()
                .read_measured_values(&mut bus, &mut Delay)
                .expect("values"),
            Measurement::default()
        );
        sensor()
            .start_measurement(&mut bus, &mut Delay)
            .expect("start");
        assert_eq!(
            sensor()
                .read_measured_values(&mut bus, &mut Delay)
                .expect("values"),
            Measurement {
                pm1_0_ng_per_m3: Some(1_200),
                pm2_5_ng_per_m3: Some(3_400),
                pm4_0_ng_per_m3: Some(4_500),
                pm10_ng_per_m3: Some(5_600),
                relative_humidity_millipercent: Some(45_120),
                temperature_millicelsius: Some(23_500),
                voc_index_milli: Some(100_500),
                nox_index_milli: Some(1_000),
            }
        );
        assert_eq!(bus.commands, [0xd304, 0x03c4, 0x0021, 0x03c4]);
    }

    #[test]
    fn converts_negative_temperatures() {
        // -10 degrees Celsius is -2000 two-hundredths.
        let raw = u16::from_be_bytes((-2_000_i16).to_be_bytes());
        assert_eq!(
            super::signed(raw).map(|value| i32::from(value) * 5),
            Some(-10_000)
        );
        assert_eq!(super::index(0x7fff), None);
        assert_eq!(super::mass_concentration(0xffff), None);
    }
}

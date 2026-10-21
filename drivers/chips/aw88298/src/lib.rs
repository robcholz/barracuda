//! Register-level driver for the AWINIC AW88298 I2S smart amplifier.
//!
//! The AW88298 takes I2S audio and drives a speaker; its registers are 16
//! bits wide, sent most significant byte first after an 8-bit register
//! address. The start-up sequence and values follow M5Stack's M5Unified
//! (`_speaker_enabled_cb_cores3`), which runs the amplifier on the CoreS3:
//!
//! | Register | Value | Meaning |
//! | --- | --- | --- |
//! | `61h` BSTCTRL2 | `0673h` | boost converter disabled |
//! | `04h` SYSCTRL | `4040h` | I2S enabled, amplifier and chip powered |
//! | `05h` SYSCTRL2 | `0008h` | no RMS, AGC, or DC-cancel hold; unmuted |
//! | `06h` I2SCTRL | `14C0h` + rate | 16-bit, 32-BCLK frames, sample rate |
//! | `0Ch` VOLCTRL | `0064h` | full volume |
//!
//! Writing `4000h` to SYSCTRL disables I2S and stops the output.

#![no_std]

use embedded_hal::i2c::I2c;

/// Seven-bit I2C address of the AW88298 with its address pins low.
pub const DEFAULT_ADDRESS: u8 = 0x36;

const SYSCTRL: u8 = 0x04;
const SYSCTRL2: u8 = 0x05;
const I2SCTRL: u8 = 0x06;
const VOLCTRL: u8 = 0x0c;
const BSTCTRL2: u8 = 0x61;

const BOOST_DISABLED: u16 = 0x0673;
const SYSCTRL_RUNNING: u16 = 0x4040;
const SYSCTRL_STOPPED: u16 = 0x4000;
const SYSCTRL2_UNMUTED: u16 = 0x0008;
/// I2SCTRL with I2SBCK=0 (32 BCLKs per frame) and 16-bit data; the low
/// bits select the sample rate.
const I2SCTRL_16_BIT: u16 = 0x14c0;
const FULL_VOLUME: u16 = 0x0064;

/// The upper sample rate of I2SCTRL rate codes 0 to 9 in units of 2205 Hz
/// (M5Unified `rate_tbl`).
const RATE_TABLE: [u32; 10] = [4, 5, 6, 8, 10, 11, 15, 20, 22, 44];

/// AW88298 register access failure with the rejected register address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aw88298Error<E> {
    /// Register being accessed when the bus operation failed.
    pub register: u8,
    /// Underlying I2C error.
    pub source: E,
}

/// An AW88298 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aw88298 {
    address: u8,
}

impl Aw88298 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Returns the I2SCTRL rate code for `sample_rate_hz`: the first table
    /// rate at or above it, as M5Unified rounds.
    #[must_use]
    pub const fn rate_code(sample_rate_hz: u32) -> u16 {
        let rate = (sample_rate_hz + 1102) / 2205;
        let mut code = 0;
        while code < RATE_TABLE.len() - 1 && rate > RATE_TABLE[code] {
            code += 1;
        }
        code as u16
    }

    /// Powers the amplifier and starts playing 16-bit stereo I2S frames at
    /// `sample_rate_hz`, at full volume.
    pub fn start<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        sample_rate_hz: u32,
    ) -> Result<(), Aw88298Error<I2C::Error>> {
        self.write_register(i2c, BSTCTRL2, BOOST_DISABLED)?;
        self.write_register(i2c, SYSCTRL, SYSCTRL_RUNNING)?;
        self.write_register(i2c, SYSCTRL2, SYSCTRL2_UNMUTED)?;
        self.write_register(
            i2c,
            I2SCTRL,
            I2SCTRL_16_BIT | Self::rate_code(sample_rate_hz),
        )?;
        self.write_register(i2c, VOLCTRL, FULL_VOLUME)
    }

    /// Disables I2S input, which silences the output.
    pub fn stop<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), Aw88298Error<I2C::Error>> {
        self.write_register(i2c, SYSCTRL, SYSCTRL_STOPPED)
    }

    fn write_register<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        register: u8,
        value: u16,
    ) -> Result<(), Aw88298Error<I2C::Error>> {
        let [high, low] = value.to_be_bytes();
        i2c.write(self.address, &[register, high, low])
            .map_err(|source| Aw88298Error { register, source })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{Aw88298, DEFAULT_ADDRESS};

    #[derive(Default)]
    struct Bus(Vec<Vec<u8>>);

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Infallible> {
            assert_eq!(address, DEFAULT_ADDRESS);
            for operation in operations {
                if let Operation::Write(bytes) = operation {
                    self.0.push(bytes.to_vec());
                }
            }
            Ok(())
        }
    }

    #[test]
    fn rounds_sample_rates_to_codes() {
        assert_eq!(Aw88298::rate_code(8_000), 0);
        assert_eq!(Aw88298::rate_code(16_000), 3);
        assert_eq!(Aw88298::rate_code(44_100), 7);
        assert_eq!(Aw88298::rate_code(48_000), 8);
        assert_eq!(Aw88298::rate_code(192_000), 9);
    }

    #[test]
    fn starts_with_m5unified_sequence_big_endian() {
        let mut bus = Bus::default();
        let amplifier = Aw88298::new(DEFAULT_ADDRESS).unwrap_or(Aw88298 { address: 0x36 });
        assert!(amplifier.start(&mut bus, 48_000).is_ok());
        assert_eq!(
            bus.0,
            [
                [0x61, 0x06, 0x73],
                [0x04, 0x40, 0x40],
                [0x05, 0x00, 0x08],
                [0x06, 0x14, 0xc8],
                [0x0c, 0x00, 0x64],
            ]
        );
        assert!(amplifier.stop(&mut bus).is_ok());
        assert_eq!(
            bus.0.last().map(Vec::as_slice),
            Some(&[0x04, 0x40, 0x00][..])
        );
        assert!(Aw88298::new(0x80).is_none());
    }
}

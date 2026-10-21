//! Register-level ES8311 audio-codec driver.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

const INITIAL_REGISTERS: &[(u8, u8)] = &[
    (0x00, 0x80),
    (0x01, 0x3f),
    (0x02, 0x00),
    (0x03, 0x10),
    (0x04, 0x10),
    (0x05, 0x00),
    (0x06, 0x03),
    (0x07, 0x00),
    (0x08, 0xff),
    (0x09, 0x0c),
    (0x0a, 0x0c),
    (0x0d, 0x01),
    (0x0e, 0x02),
    (0x12, 0x00),
    (0x13, 0x10),
    (0x1c, 0x6a),
    (0x32, 0xbf),
    (0x37, 0x08),
];

/// Where the codec takes its master clock from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClockSource {
    /// The MCLK pin, at 256 times the sample rate.
    #[default]
    Mclk,
    /// The bit clock of 16-bit stereo frames (32 times the sample rate),
    /// multiplied by eight, for Boards that wire no MCLK. Register 01h keeps
    /// every clock of the MCLK setting and sets bit 7, MCLK from BCLK, as
    /// M5Unified's Cardputer-ADV speaker (`0xB5`) and microphone (`0xBA`)
    /// settings do for their halves; 02h is MULT_PRE=3 (`0x18`).
    Bclk,
}

impl ClockSource {
    /// Clock manager registers 01h and 02h for this source.
    const fn registers(self) -> [(u8, u8); 2] {
        match self {
            Self::Mclk => [(0x01, 0x3f), (0x02, 0x00)],
            Self::Bclk => [(0x01, 0xbf), (0x02, 0x18)],
        }
    }
}

/// An ES8311 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Es8311 {
    address: u8,
}

/// ES8311 register access failure with the rejected register address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Es8311Error<E> {
    /// Register being accessed when the bus operation failed.
    pub register: u8,
    /// Underlying I2C error.
    pub source: E,
}

impl Es8311 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Resets and programs the codec's 16-bit stereo signal path, clocked
    /// from its MCLK pin.
    pub fn initialize<I2C: I2c, DELAY: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut DELAY,
    ) -> Result<(), Es8311Error<I2C::Error>> {
        self.initialize_with_clock(i2c, delay, ClockSource::Mclk)
    }

    /// Resets and programs the codec's 16-bit stereo signal path, clocked
    /// from `clock`.
    pub fn initialize_with_clock<I2C: I2c, DELAY: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut DELAY,
        clock: ClockSource,
    ) -> Result<(), Es8311Error<I2C::Error>> {
        self.write_register(i2c, 0x00, 0x1f)?;
        delay.delay_ms(20);
        self.write_register(i2c, 0x00, 0x00)?;
        let clock = clock.registers();
        for &(register, value) in INITIAL_REGISTERS {
            let value = clock
                .iter()
                .find(|(clock_register, _)| *clock_register == register)
                .map_or(value, |&(_, clock_value)| clock_value);
            self.write_register(i2c, register, value)?;
        }
        Ok(())
    }

    /// Writes the DAC volume register.
    pub fn set_dac_volume<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        volume: u8,
    ) -> Result<(), Es8311Error<I2C::Error>> {
        self.write_register(i2c, 0x32, volume)
    }

    fn write_register<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        register: u8,
        value: u8,
    ) -> Result<(), Es8311Error<I2C::Error>> {
        i2c.write(self.address, &[register, value])
            .map_err(|source| Es8311Error { register, source })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{ClockSource, Es8311};

    #[derive(Default)]
    struct Bus(Vec<(u8, u8)>);

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            _: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Infallible> {
            for operation in operations {
                if let Operation::Write([register, value]) = operation {
                    self.0.push((*register, *value));
                }
            }
            Ok(())
        }
    }

    struct Delay;

    impl DelayNs for Delay {
        fn delay_ns(&mut self, _: u32) {}
    }

    fn clock_registers(clock: ClockSource) -> Vec<(u8, u8)> {
        let mut bus = Bus::default();
        let codec = Es8311::new(0x18).unwrap_or_else(|| unreachable_address());
        let _ = codec.initialize_with_clock(&mut bus, &mut Delay, clock);
        bus.0
            .into_iter()
            .filter(|(register, _)| matches!(register, 0x01 | 0x02))
            .collect()
    }

    fn unreachable_address() -> Es8311 {
        Es8311 { address: 0x18 }
    }

    #[test]
    fn clocks_from_mclk_or_bclk() {
        assert_eq!(
            clock_registers(ClockSource::Mclk),
            [(0x01, 0x3f), (0x02, 0x00)]
        );
        assert_eq!(
            clock_registers(ClockSource::Bclk),
            [(0x01, 0xbf), (0x02, 0x18)]
        );
    }
}

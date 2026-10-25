//! Register-level driver for the Texas Instruments INA3221 three-channel
//! shunt and bus voltage monitor.
//!
//! Registers follow the INA3221 datasheet (SBOS576, section 8.6): channel
//! `n` (0 to 2) has its shunt voltage at `01h + 2n` and its bus voltage at
//! `02h + 2n`, both big-endian two's-complement words whose value sits in
//! bits 15:3, at 40 µV and 8 mV per step; `FEh` holds the manufacturer ID
//! `5449h` and `FFh` the die ID `3220h`. The driver reads the power-on
//! configuration (all channels, continuous conversion) and never writes, as
//! M5Stack's M5Unified `INA3221_Class` does.

#![no_std]

use embedded_hal::i2c::I2c;

/// The address with A0 tied to GND.
pub const DEFAULT_ADDRESS: u8 = 0x40;

const FIRST_SHUNT_VOLTAGE: u8 = 0x01;
const MANUFACTURER_ID: u8 = 0xfe;
const DIE_ID: u8 = 0xff;
const TEXAS_INSTRUMENTS: u16 = 0x5449;
const INA3221: u16 = 0x3220;
/// Number of measurement channels.
pub const CHANNELS: u8 = 3;

/// One channel's voltages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelReading {
    /// Signed shunt voltage in microvolts, positive from IN+ to IN-.
    pub shunt_microvolts: i32,
    /// Bus voltage on IN- in millivolts.
    pub bus_millivolts: i32,
}

/// INA3221 failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// The channel is outside 0 to 2.
    InvalidChannel,
}

/// An INA3221 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ina3221 {
    address: u8,
}

impl Ina3221 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Reports whether the manufacturer and die IDs name an INA3221.
    ///
    /// # Errors
    ///
    /// Returns the bus error when nothing acknowledges the read.
    pub fn is_present<I2C: I2c>(&self, i2c: &mut I2C) -> Result<bool, I2C::Error> {
        Ok(self.read(i2c, MANUFACTURER_ID)? == TEXAS_INSTRUMENTS
            && self.read(i2c, DIE_ID)? == INA3221)
    }

    /// Reads one channel's shunt and bus voltages.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidChannel`] or the bus error.
    pub fn read_channel<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        channel: u8,
    ) -> Result<ChannelReading, Error<I2C::Error>> {
        if channel >= CHANNELS {
            return Err(Error::InvalidChannel);
        }
        let shunt = FIRST_SHUNT_VOLTAGE.saturating_add(channel.saturating_mul(2));
        let bus = shunt.saturating_add(1);
        let shunt = self.read(i2c, shunt).map_err(Error::Bus)?;
        let bus = self.read(i2c, bus).map_err(Error::Bus)?;
        Ok(ChannelReading {
            shunt_microvolts: (i32::from(shunt.cast_signed()) >> 3).saturating_mul(40),
            bus_millivolts: (i32::from(bus.cast_signed()) >> 3).saturating_mul(8),
        })
    }

    fn read<I2C: I2c>(&self, i2c: &mut I2C, register: u8) -> Result<u16, I2C::Error> {
        let mut value = [0; 2];
        i2c.write_read(self.address, &[register], &mut value)?;
        Ok(u16::from_be_bytes(value))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use core::convert::Infallible;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{DEFAULT_ADDRESS, Error, Ina3221};

    struct Chip;

    impl ErrorType for Chip {
        type Error = Infallible;
    }

    impl I2c for Chip {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, DEFAULT_ADDRESS);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        assert_eq!(bytes.len(), 1, "the driver only reads");
                        register = bytes[0];
                    }
                    Operation::Read(bytes) => {
                        let value: u16 = match register {
                            // CH1: -1.2 mV across the shunt, 3.952 V bus.
                            0x01 => (-30_i16 << 3).cast_unsigned(),
                            0x02 => 494 << 3,
                            // CH3: +40.96 mV, 12 V.
                            0x05 => 1_024 << 3,
                            0x06 => 1_500 << 3,
                            0xfe => 0x5449,
                            0xff => 0x3220,
                            _ => 0,
                        };
                        bytes.copy_from_slice(&value.to_be_bytes());
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn identifies_the_chip_and_scales_both_voltages() {
        let chip = Ina3221::new(DEFAULT_ADDRESS).expect("valid address");
        assert!(chip.is_present(&mut Chip).expect("ids"));
        let first = chip.read_channel(&mut Chip, 0).expect("CH1");
        assert_eq!(first.shunt_microvolts, -1_200);
        assert_eq!(first.bus_millivolts, 3_952);
        let third = chip.read_channel(&mut Chip, 2).expect("CH3");
        assert_eq!(third.shunt_microvolts, 40_960);
        assert_eq!(third.bus_millivolts, 12_000);
        assert!(matches!(
            chip.read_channel(&mut Chip, 3),
            Err(Error::InvalidChannel)
        ));
    }
}

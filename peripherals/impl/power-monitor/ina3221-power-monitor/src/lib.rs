//! One INA3221 channel as a voltage, current, and power monitor.
//!
//! The INA3221 measures three shunts at once. Each Board device names one
//! channel and the resistance of its shunt; the current is the shunt voltage
//! (40 µV steps) divided by that resistance, and the power the product of the
//! current and the bus voltage (8 mV steps) on the channel's IN- side.
//! Positive current flows from IN+ to IN-.
//!
//! # Shared ownership
//!
//! Devices on the channels of one chip share it through their own views of
//! the bus. Each reads only its channel's two voltage registers and the ID
//! registers, and none writes the chip, which keeps its power-on
//! configuration: all channels, continuous conversion.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_ina3221::{CHANNELS, Error as Ina3221Error, Ina3221};
use barracuda_peripheral::{PeripheralImplementation, power::PowerMonitor};
use embedded_hal::i2c::I2c;

pub use barracuda_peripheral::power::PowerMeasurement;

/// Board-owned chip address, channel, and shunt resistance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ina3221Config {
    address: u8,
    channel: u8,
    shunt_micro_ohms: u32,
}

impl Ina3221Config {
    /// Creates configuration for `channel` (0 for CH1 to 2 for CH3) of the
    /// INA3221 at `address`, measured through `shunt_micro_ohms`.
    #[must_use]
    pub const fn new(address: u8, channel: u8, shunt_micro_ohms: u32) -> Self {
        Self {
            address,
            channel,
            shunt_micro_ohms,
        }
    }
}

/// Move-only resources consumed by the monitor implementation.
pub struct Ina3221Bindings<I2C> {
    i2c: I2C,
}

impl<I2C> Ina3221Bindings<I2C> {
    /// Wraps this device's own view of the chip's bus.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// INA3221 initialization failure.
#[derive(Debug)]
pub enum Ina3221InitError<BusError> {
    /// The address, channel, or shunt resistance is invalid.
    InvalidConfig,
    /// A register read failed.
    Bus(BusError),
    /// The ID registers do not name an INA3221.
    UnexpectedDevice,
}

/// Initialized monitor of one INA3221 channel.
pub struct Ina3221PowerMonitor<I2C> {
    i2c: I2C,
    chip: Ina3221,
    channel: u8,
    shunt_micro_ohms: u32,
}

/// Static factory used by generated Board composition.
pub struct Ina3221PowerMonitorImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for Ina3221PowerMonitorImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Ina3221Bindings<I2C>;
    type Config = Ina3221Config;
    type Peripheral = Ina3221PowerMonitor<I2C>;
    type Error = Ina3221InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip = Ina3221::new(config.address).ok_or(Ina3221InitError::InvalidConfig)?;
        if config.channel >= CHANNELS || config.shunt_micro_ohms == 0 {
            return Err(Ina3221InitError::InvalidConfig);
        }
        if !chip
            .is_present(&mut bindings.i2c)
            .map_err(Ina3221InitError::Bus)?
        {
            return Err(Ina3221InitError::UnexpectedDevice);
        }
        Ok(Ina3221PowerMonitor {
            i2c: bindings.i2c,
            chip,
            channel: config.channel,
            shunt_micro_ohms: config.shunt_micro_ohms,
        })
    }
}

impl<I2C: I2c> PowerMonitor for Ina3221PowerMonitor<I2C> {
    type Error = Ina3221Error<I2C::Error>;

    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        let reading = self.chip.read_channel(&mut self.i2c, self.channel)?;
        // The bus voltage register is signed, but IN- never sits below ground.
        let bus_microvolts = u64::try_from(reading.bus_millivolts)
            .unwrap_or_default()
            .saturating_mul(1_000);
        let shunt_nanovolts = i64::from(reading.shunt_microvolts).saturating_mul(1_000);
        let current_microamps = shunt_nanovolts
            .saturating_mul(1_000)
            .checked_div(i64::from(self.shunt_micro_ohms))
            .unwrap_or_default();
        let power_microwatts = i64::try_from(bus_microvolts)
            .unwrap_or(i64::MAX)
            .saturating_mul(current_microamps)
            .checked_div(1_000_000)
            .unwrap_or_default();
        Ok(PowerMeasurement {
            bus_microvolts,
            shunt_nanovolts,
            current_microamps,
            power_microwatts,
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_peripheral::{PeripheralImplementation, power::PowerMonitor};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{
        Ina3221Bindings, Ina3221Config, Ina3221InitError, Ina3221PowerMonitorImplementation,
        PowerMeasurement,
    };

    /// An INA3221 at 41h: CH2 carries 5.016 V and 4.0 mV across its shunt.
    struct Chip {
        die: u16,
    }

    impl ErrorType for Chip {
        type Error = Infallible;
    }

    impl I2c for Chip {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x41);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        assert_eq!(bytes.len(), 1, "the monitor only reads");
                        register = bytes[0];
                    }
                    Operation::Read(bytes) => {
                        let value: u16 = match register {
                            0x03 => 100 << 3,
                            0x04 => 627 << 3,
                            0xfe => 0x5449,
                            0xff => self.die,
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
    fn measures_its_channel_through_the_shunt() {
        let mut monitor = block_on(Ina3221PowerMonitorImplementation::initialize(
            Ina3221Bindings::new(Chip { die: 0x3220 }),
            Ina3221Config::new(0x41, 1, 10_000),
        ))
        .expect("start");
        // 4.0 mV across 10 mOhm is 400 mA; at 5.016 V, 2.0064 W.
        assert_eq!(
            monitor.measure().expect("measure"),
            PowerMeasurement {
                bus_microvolts: 5_016_000,
                shunt_nanovolts: 4_000_000,
                current_microamps: 400_000,
                power_microwatts: 2_006_400,
            }
        );
    }

    #[test]
    fn refuses_another_chip_and_a_bad_channel() {
        assert!(matches!(
            block_on(Ina3221PowerMonitorImplementation::initialize(
                Ina3221Bindings::new(Chip { die: 0x2260 }),
                Ina3221Config::new(0x41, 0, 10_000),
            )),
            Err(Ina3221InitError::UnexpectedDevice)
        ));
        assert!(matches!(
            block_on(Ina3221PowerMonitorImplementation::initialize(
                Ina3221Bindings::new(Chip { die: 0x3220 }),
                Ina3221Config::new(0x41, 3, 10_000),
            )),
            Err(Ina3221InitError::InvalidConfig)
        ));
    }
}

//! Battery-level monitor for the Injoinic IP5306 power-bank SoC.
//!
//! The IP5306 reports its battery level in 25 % steps and whether it is
//! charging, but measures nothing it would report over I2C: [`measure`]
//! fails with [`Ip5306Error::NoMeasurement`] and [`battery`] carries the
//! reading. The implementation never writes to the chip, so the power-path
//! settings that keep the boost converter on stay with the Board's firmware.
//!
//! [`measure`]: PowerMonitor::measure
//! [`battery`]: PowerMonitor::battery

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_ip5306::Ip5306;
use barracuda_peripheral::{
    PeripheralImplementation,
    power::{BatteryState, PowerMeasurement, PowerMonitor},
};
use embedded_hal::i2c::I2c;

/// Board-owned IP5306 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ip5306Config {
    address: u8,
}

impl Ip5306Config {
    /// Creates a configuration for the chip at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Move-only resources consumed by the implementation.
pub struct Ip5306Bindings<I2C> {
    i2c: I2C,
}

impl<I2C> Ip5306Bindings<I2C> {
    /// Wraps an I2C device view.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum Ip5306InitError<BusError> {
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress(u8),
    /// The chip did not answer the level read.
    Bus(BusError),
}

/// Reading failure.
#[derive(Debug)]
pub enum Ip5306Error<BusError> {
    /// The IP5306 measures no voltage or current; read the battery state.
    NoMeasurement,
    /// A register read failed.
    Bus(BusError),
}

/// Initialized IP5306 monitor.
pub struct Ip5306PowerMonitor<I2C> {
    i2c: I2C,
    chip: Ip5306,
}

/// Static factory used by generated Board composition.
pub struct Ip5306PowerMonitorImplementation<I2C>(PhantomData<I2C>);

impl<I2C> PeripheralImplementation for Ip5306PowerMonitorImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Ip5306Bindings<I2C>;
    type Config = Ip5306Config;
    type Peripheral = Ip5306PowerMonitor<I2C>;
    type Error = Ip5306InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip =
            Ip5306::new(config.address).ok_or(Ip5306InitError::InvalidAddress(config.address))?;
        chip.battery_percent(&mut bindings.i2c)
            .map_err(Ip5306InitError::Bus)?;
        Ok(Ip5306PowerMonitor {
            i2c: bindings.i2c,
            chip,
        })
    }
}

impl<I2C: I2c> PowerMonitor for Ip5306PowerMonitor<I2C> {
    type Error = Ip5306Error<I2C::Error>;

    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        Err(Ip5306Error::NoMeasurement)
    }

    fn battery(&mut self) -> Result<Option<BatteryState>, Self::Error> {
        let percent = self
            .chip
            .battery_percent(&mut self.i2c)
            .map_err(Ip5306Error::Bus)?;
        let charging = self
            .chip
            .is_charging(&mut self.i2c)
            .map_err(Ip5306Error::Bus)?;
        Ok(Some(BatteryState { percent, charging }))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use core::convert::Infallible;

    use barracuda_peripheral::{
        PeripheralImplementation,
        power::{BatteryState, PowerMonitor},
    };
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{Ip5306Bindings, Ip5306Config, Ip5306Error, Ip5306PowerMonitorImplementation};

    struct Chip;

    impl ErrorType for Chip {
        type Error = Infallible;
    }

    impl I2c for Chip {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => register = bytes[0],
                    Operation::Read(bytes) => {
                        bytes[0] = match register {
                            // Charging, not complete, three quarters full.
                            0x70 => 0x08,
                            0x78 => 0x8f,
                            _ => 0,
                        };
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn reports_the_level_and_refuses_a_measurement() {
        let mut monitor = block_on(Ip5306PowerMonitorImplementation::initialize(
            Ip5306Bindings::new(Chip),
            Ip5306Config::new(0x75),
        ))
        .expect("initialize");
        assert!(matches!(monitor.measure(), Err(Ip5306Error::NoMeasurement)));
        assert_eq!(
            monitor.battery().expect("battery"),
            Some(BatteryState {
                percent: 75,
                charging: true,
            })
        );
    }
}

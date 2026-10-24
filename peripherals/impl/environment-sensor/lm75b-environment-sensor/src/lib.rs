//! Environment sensor over an NXP LM75B: temperature.
//!
//! Initialization takes the converter out of shutdown and checks the
//! configuration reads back, so a Board whose sensor does not answer fails
//! to start. The LM75B converts continuously; a read returns its latest
//! conversion without waiting.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_lm75b::Lm75b;
use barracuda_peripheral::{
    PeripheralImplementation,
    environment::{EnvironmentReading, EnvironmentSensor, Quantities, Quantity},
};
use embedded_hal::i2c::I2c;

/// Board-owned sensor configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lm75bConfig {
    address: u8,
}

impl Lm75bConfig {
    /// Creates a configuration for one seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Move-only resources consumed by the implementation.
pub struct Lm75bBindings<I2C> {
    i2c: I2C,
}

impl<I2C> Lm75bBindings<I2C> {
    /// Wraps the I2C device view.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum Lm75bInitError<BusError> {
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress(u8),
    /// The sensor did not answer.
    Bus(BusError),
    /// The configuration register did not read back as written.
    NotAnLm75b,
}

/// Initialized LM75B environment sensor.
pub struct Lm75bEnvironmentSensor<I2C> {
    i2c: I2C,
    chip: Lm75b,
}

/// Static factory used by generated Board composition.
pub struct Lm75bEnvironmentSensorImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for Lm75bEnvironmentSensorImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Lm75bBindings<I2C>;
    type Config = Lm75bConfig;
    type Peripheral = Lm75bEnvironmentSensor<I2C>;
    type Error = Lm75bInitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip =
            Lm75b::new(config.address).ok_or(Lm75bInitError::InvalidAddress(config.address))?;
        if !chip.start(&mut bindings.i2c).map_err(Lm75bInitError::Bus)? {
            return Err(Lm75bInitError::NotAnLm75b);
        }
        Ok(Lm75bEnvironmentSensor {
            i2c: bindings.i2c,
            chip,
        })
    }
}

impl<I2C> EnvironmentSensor for Lm75bEnvironmentSensor<I2C>
where
    I2C: I2c,
{
    type Error = I2C::Error;

    fn quantities(&self) -> Quantities {
        Quantities::EMPTY.with(Quantity::Temperature)
    }

    async fn read(&mut self) -> Result<EnvironmentReading, Self::Error> {
        Ok(EnvironmentReading {
            temperature_millicelsius: Some(self.chip.temperature_millicelsius(&mut self.i2c)?),
            ..EnvironmentReading::EMPTY
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    use core::convert::Infallible;

    use barracuda_peripheral::{
        PeripheralImplementation,
        environment::{EnvironmentSensor, Quantity},
    };
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{Lm75bBindings, Lm75bConfig, Lm75bEnvironmentSensorImplementation, Lm75bInitError};

    /// Temperature register 1880h (24.5 degrees Celsius); configuration
    /// stuck at `stuck` when set, as a different chip at the address may.
    struct Sensor {
        pointer: u8,
        configuration: u8,
        stuck: Option<u8>,
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
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        self.pointer = bytes[0];
                        if let [0x01, value] = bytes[..] {
                            self.configuration = self.stuck.unwrap_or(value);
                        }
                    }
                    Operation::Read(buffer) => match self.pointer {
                        0x00 => buffer.copy_from_slice(&[0x18, 0x80]),
                        _ => buffer[0] = self.configuration,
                    },
                }
            }
            Ok(())
        }
    }

    #[test]
    fn reports_temperature() {
        let mut sensor = block_on(Lm75bEnvironmentSensorImplementation::initialize(
            Lm75bBindings::new(Sensor {
                pointer: 0,
                configuration: 0x01,
                stuck: None,
            }),
            Lm75bConfig::new(0x48),
        ))
        .expect("initialize");
        assert_eq!(sensor.quantities().len(), 1);
        assert!(sensor.quantities().contains(Quantity::Temperature));
        let reading = block_on(sensor.read()).expect("read");
        assert_eq!(reading.temperature_millicelsius, Some(24_500));
        assert_eq!(reading.relative_humidity_millipercent, None);
    }

    #[test]
    fn refuses_a_register_that_does_not_read_back() {
        let result = block_on(Lm75bEnvironmentSensorImplementation::initialize(
            Lm75bBindings::new(Sensor {
                pointer: 0,
                configuration: 0,
                stuck: Some(0x1f),
            }),
            Lm75bConfig::new(0x48),
        ));
        assert!(matches!(result, Err(Lm75bInitError::NotAnLm75b)));
    }
}

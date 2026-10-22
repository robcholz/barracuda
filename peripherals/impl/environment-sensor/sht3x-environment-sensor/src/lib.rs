//! Environment sensor over a Sensirion SHT30, SHT31, or SHT35: air
//! temperature and relative humidity.
//!
//! Initialization resets the sensor and reads its status register, so a
//! Board whose sensor does not answer fails to start. Each read takes one
//! high-repeatability single-shot measurement, which waits up to 16 ms.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_sht3x::{Error as ChipError, Sht3x};
use barracuda_peripheral::{
    PeripheralImplementation,
    environment::{EnvironmentReading, EnvironmentSensor, Quantities, Quantity},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

/// Board-owned sensor configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sht3xConfig {
    address: u8,
}

impl Sht3xConfig {
    /// Creates a configuration for one seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Move-only resources consumed by the implementation.
pub struct Sht3xBindings<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
}

impl<I2C, DELAY> Sht3xBindings<I2C, DELAY> {
    /// Combines the I2C device view and the delay source.
    #[must_use]
    pub const fn new(i2c: I2C, delay: DELAY) -> Self {
        Self { i2c, delay }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum Sht3xInitError<BusError> {
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress(u8),
    /// The sensor did not answer its reset or status read.
    Sensor(ChipError<BusError>),
}

/// Initialized SHT3x environment sensor.
pub struct Sht3xEnvironmentSensor<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
    chip: Sht3x,
}

/// Static factory used by generated Board composition.
pub struct Sht3xEnvironmentSensorImplementation<I2C, DELAY>(PhantomData<fn() -> (I2C, DELAY)>);

impl<I2C, DELAY> PeripheralImplementation for Sht3xEnvironmentSensorImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Sht3xBindings<I2C, DELAY>;
    type Config = Sht3xConfig;
    type Peripheral = Sht3xEnvironmentSensor<I2C, DELAY>;
    type Error = Sht3xInitError<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let Sht3xBindings { mut i2c, mut delay } = bindings;
        let chip =
            Sht3x::new(config.address).ok_or(Sht3xInitError::InvalidAddress(config.address))?;
        chip.soft_reset(&mut i2c, &mut delay)
            .map_err(|error| Sht3xInitError::Sensor(ChipError::Bus(error)))?;
        chip.read_status(&mut i2c, &mut delay)
            .map_err(Sht3xInitError::Sensor)?;
        Ok(Sht3xEnvironmentSensor { i2c, delay, chip })
    }
}

impl<I2C, DELAY> EnvironmentSensor for Sht3xEnvironmentSensor<I2C, DELAY>
where
    I2C: I2c,
    DELAY: DelayNs,
{
    type Error = ChipError<I2C::Error>;

    fn quantities(&self) -> Quantities {
        Quantities::EMPTY
            .with(Quantity::Temperature)
            .with(Quantity::RelativeHumidity)
    }

    async fn read(&mut self) -> Result<EnvironmentReading, Self::Error> {
        let measurement = self.chip.measure(&mut self.i2c, &mut self.delay)?;
        Ok(EnvironmentReading {
            temperature_millicelsius: Some(measurement.temperature_millicelsius),
            relative_humidity_millipercent: Some(measurement.relative_humidity_millipercent),
            ..EnvironmentReading::EMPTY
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    use core::convert::Infallible;

    use barracuda_driver_sht3x::crc8;
    use barracuda_peripheral::{
        PeripheralImplementation,
        environment::{EnvironmentSensor, Quantity},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{Sht3xBindings, Sht3xConfig, Sht3xEnvironmentSensorImplementation};

    /// Answers the status read and a measurement of 25 degrees Celsius and
    /// 50 percent.
    struct Sensor {
        last: [u8; 2],
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
                    Operation::Write(bytes) => self.last = [bytes[0], bytes[1]],
                    Operation::Read(buffer) => {
                        let words: &[u16] = match self.last {
                            [0x24, 0x00] => &[0x6666, 0x8000],
                            _ => &[0x0000],
                        };
                        for (chunk, word) in buffer.as_chunks_mut::<3>().0.iter_mut().zip(words) {
                            let [high, low] = word.to_be_bytes();
                            chunk.copy_from_slice(&[high, low, crc8(&[high, low])]);
                        }
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

    #[test]
    fn reports_temperature_and_humidity() {
        let mut sensor = block_on(Sht3xEnvironmentSensorImplementation::initialize(
            Sht3xBindings::new(Sensor { last: [0; 2] }, Delay),
            Sht3xConfig::new(0x44),
        ))
        .expect("initialize");
        let quantities = sensor.quantities();
        assert!(quantities.contains(Quantity::Temperature));
        assert!(quantities.contains(Quantity::RelativeHumidity));
        assert_eq!(quantities.len(), 2);
        let reading = block_on(sensor.read()).expect("read");
        assert_eq!(reading.temperature_millicelsius, Some(25_000));
        assert_eq!(reading.relative_humidity_millipercent, Some(50_000));
        assert_eq!(reading.quantities(), quantities);
    }

    #[test]
    fn rejects_a_non_seven_bit_address() {
        let result = block_on(Sht3xEnvironmentSensorImplementation::initialize(
            Sht3xBindings::new(Sensor { last: [0; 2] }, Delay),
            Sht3xConfig::new(0x88),
        ));
        assert!(result.is_err());
    }
}

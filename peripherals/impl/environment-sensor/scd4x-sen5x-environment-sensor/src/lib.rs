//! One environment sensor composed of a Sensirion SCD40/SCD41 CO2 sensor
//! and a SEN50/SEN54/SEN55 environmental sensor node on one I2C bus.
//!
//! The M5Stack AirQ carries both: a SEN55 at 69h, whose supply GPIO10
//! switches on while low, and an SCD40 at 62h, on SDA GPIO11 / SCL GPIO12.
//! A Board has one device per peripheral kind, so this implementation
//! presents the pair as one device whose quantities are the union of the
//! two chips': CO2 from the SCD4x; particulate matter, the VOC and NOx
//! indices, temperature, and humidity from the SEN5x, with the SCD4x's
//! temperature and humidity standing in for a SEN50, which measures
//! neither.
//!
//! Initialization switches the supply on, stops the SCD4x's periodic
//! measurement (it survives a host reset) and reads its serial number,
//! resets the SEN5x and reads its product name, then starts both, so a
//! Board where either chip does not answer fails to start. Both then
//! measure continuously: the SEN5x once a second, the SCD4x every five
//! seconds. A read takes the SEN5x's latest values and, when the SCD4x has
//! a new sample, that sample; it keeps the last SCD4x sample between
//! samples, and CO2 is `None` until the first one, five seconds after
//! start.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_scd4x::{Error as Scd4xError, Measurement as Scd4xMeasurement, Scd4x};
use barracuda_driver_sen5x::{Error as Sen5xError, Model, Sen5x};
use barracuda_peripheral::{
    PeripheralImplementation,
    environment::{EnvironmentReading, EnvironmentSensor, Quantities, Quantity},
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c};

/// Time for both chips to answer after their supply switches on: the SCD4x
/// needs 30 ms (`scd4x_wake_up`), and the SEN5x reference driver waits up
/// to 200 ms after a reset; 200 ms covers both.
const POWER_ON_MS: u32 = 200;

/// Board-owned addresses and supply polarity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scd4xSen5xConfig {
    scd4x_address: u8,
    sen5x_address: u8,
    power_active_high: bool,
}

impl Scd4xSen5xConfig {
    /// Creates a configuration.
    #[must_use]
    pub const fn new(scd4x_address: u8, sen5x_address: u8, power_active_high: bool) -> Self {
        Self {
            scd4x_address,
            sen5x_address,
            power_active_high,
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct Scd4xSen5xBindings<I2C, POWER, DELAY> {
    i2c: I2C,
    power_enable: Option<POWER>,
    delay: DELAY,
}

impl<I2C, POWER, DELAY> Scd4xSen5xBindings<I2C, POWER, DELAY> {
    /// Combines the shared I2C device view, the optional supply switch, and
    /// the delay source.
    #[must_use]
    pub const fn new(i2c: I2C, power_enable: Option<POWER>, delay: DELAY) -> Self {
        Self {
            i2c,
            power_enable,
            delay,
        }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum Scd4xSen5xInitError<BusError, PinError> {
    /// A configured address is not a seven-bit I2C address.
    InvalidAddress(u8),
    /// The supply switch did not change state.
    Power(PinError),
    /// The SCD4x did not answer.
    Scd4x(Scd4xError<BusError>),
    /// The SEN5x did not answer or is another product.
    Sen5x(Sen5xError<BusError>),
}

/// Measurement failure of one of the chips.
#[derive(Debug)]
pub enum Scd4xSen5xError<BusError> {
    /// The SCD4x read failed.
    Scd4x(Scd4xError<BusError>),
    /// The SEN5x read failed.
    Sen5x(Sen5xError<BusError>),
}

/// The initialized pair.
pub struct Scd4xSen5xEnvironmentSensor<I2C, POWER, DELAY> {
    i2c: I2C,
    delay: DELAY,
    scd4x: Scd4x,
    sen5x: Sen5x,
    model: Model,
    /// The SCD4x's latest sample; it reports one every five seconds.
    carbon_dioxide: Option<Scd4xMeasurement>,
    /// Held so the supply stays on for the sensor's lifetime.
    _power_enable: Option<POWER>,
}

impl<I2C, POWER, DELAY> Scd4xSen5xEnvironmentSensor<I2C, POWER, DELAY> {
    /// Returns the SEN5x variant that answered.
    #[must_use]
    pub const fn sen5x_model(&self) -> Model {
        self.model
    }
}

/// The bindings' types, held without owning a value of them.
type Parts<I2C, POWER, DELAY> = PhantomData<fn() -> (I2C, POWER, DELAY)>;

/// Static factory used by generated Board composition.
pub struct Scd4xSen5xEnvironmentSensorImplementation<I2C, POWER, DELAY>(Parts<I2C, POWER, DELAY>);

impl<I2C, POWER, DELAY> PeripheralImplementation
    for Scd4xSen5xEnvironmentSensorImplementation<I2C, POWER, DELAY>
where
    I2C: I2c + 'static,
    POWER: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Scd4xSen5xBindings<I2C, POWER, DELAY>;
    type Config = Scd4xSen5xConfig;
    type Peripheral = Scd4xSen5xEnvironmentSensor<I2C, POWER, DELAY>;
    type Error = Scd4xSen5xInitError<I2C::Error, POWER::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let Scd4xSen5xBindings {
            mut i2c,
            mut power_enable,
            mut delay,
        } = bindings;
        let scd4x = Scd4x::new(config.scd4x_address)
            .ok_or(Scd4xSen5xInitError::InvalidAddress(config.scd4x_address))?;
        let sen5x = Sen5x::new(config.sen5x_address)
            .ok_or(Scd4xSen5xInitError::InvalidAddress(config.sen5x_address))?;
        if let Some(power) = power_enable.as_mut() {
            power
                .set_state(config.power_active_high.into())
                .map_err(Scd4xSen5xInitError::Power)?;
            delay.delay_ms(POWER_ON_MS);
        }

        scd4x
            .stop_periodic_measurement(&mut i2c, &mut delay)
            .map_err(|error| Scd4xSen5xInitError::Scd4x(Scd4xError::Bus(error)))?;
        scd4x
            .serial_number(&mut i2c, &mut delay)
            .map_err(Scd4xSen5xInitError::Scd4x)?;
        sen5x
            .reset(&mut i2c, &mut delay)
            .map_err(|error| Scd4xSen5xInitError::Sen5x(Sen5xError::Bus(error)))?;
        let model = sen5x
            .model(&mut i2c, &mut delay)
            .map_err(Scd4xSen5xInitError::Sen5x)?;
        scd4x
            .start_periodic_measurement(&mut i2c)
            .map_err(|error| Scd4xSen5xInitError::Scd4x(Scd4xError::Bus(error)))?;
        sen5x
            .start_measurement(&mut i2c, &mut delay)
            .map_err(|error| Scd4xSen5xInitError::Sen5x(Sen5xError::Bus(error)))?;

        Ok(Scd4xSen5xEnvironmentSensor {
            i2c,
            delay,
            scd4x,
            sen5x,
            model,
            carbon_dioxide: None,
            _power_enable: power_enable,
        })
    }
}

impl<I2C, POWER, DELAY> EnvironmentSensor for Scd4xSen5xEnvironmentSensor<I2C, POWER, DELAY>
where
    I2C: I2c,
    DELAY: DelayNs,
{
    type Error = Scd4xSen5xError<I2C::Error>;

    fn quantities(&self) -> Quantities {
        let mut quantities = Quantities::EMPTY
            .with(Quantity::Co2)
            .with(Quantity::Temperature)
            .with(Quantity::RelativeHumidity)
            .with(Quantity::Pm1_0)
            .with(Quantity::Pm2_5)
            .with(Quantity::Pm4_0)
            .with(Quantity::Pm10);
        if self.model.has_gas_and_climate() {
            quantities = quantities.with(Quantity::VocIndex);
        }
        if self.model.has_nox() {
            quantities = quantities.with(Quantity::NoxIndex);
        }
        quantities
    }

    async fn read(&mut self) -> Result<EnvironmentReading, Self::Error> {
        let particulate = self
            .sen5x
            .read_measured_values(&mut self.i2c, &mut self.delay)
            .map_err(Scd4xSen5xError::Sen5x)?;
        if self
            .scd4x
            .data_ready(&mut self.i2c, &mut self.delay)
            .map_err(Scd4xSen5xError::Scd4x)?
        {
            self.carbon_dioxide = Some(
                self.scd4x
                    .read_measurement(&mut self.i2c, &mut self.delay)
                    .map_err(Scd4xSen5xError::Scd4x)?,
            );
        }
        let sen5x = EnvironmentReading {
            temperature_millicelsius: particulate.temperature_millicelsius,
            relative_humidity_millipercent: particulate.relative_humidity_millipercent,
            pm1_0_ng_per_m3: particulate.pm1_0_ng_per_m3,
            pm2_5_ng_per_m3: particulate.pm2_5_ng_per_m3,
            pm4_0_ng_per_m3: particulate.pm4_0_ng_per_m3,
            pm10_ng_per_m3: particulate.pm10_ng_per_m3,
            voc_index_milli: particulate.voc_index_milli,
            nox_index_milli: particulate.nox_index_milli,
            ..EnvironmentReading::EMPTY
        };
        let scd4x = self
            .carbon_dioxide
            .map_or(EnvironmentReading::EMPTY, |sample| EnvironmentReading {
                co2_ppm: Some(u32::from(sample.co2_ppm)),
                temperature_millicelsius: Some(sample.temperature_millicelsius),
                relative_humidity_millipercent: Some(sample.relative_humidity_millipercent),
                ..EnvironmentReading::EMPTY
            });
        Ok(sen5x.or(scd4x))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::{rc::Rc, vec, vec::Vec};

    use barracuda_driver_scd4x::crc8;
    use barracuda_driver_sen5x::Model;
    use barracuda_peripheral::{
        PeripheralImplementation,
        environment::{EnvironmentSensor, Quantity},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        digital::{ErrorType as PinErrorType, OutputPin},
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{
        Scd4xSen5xBindings, Scd4xSen5xConfig, Scd4xSen5xEnvironmentSensorImplementation,
        Scd4xSen5xInitError,
    };

    /// The AirQ's bus: an SCD40 at 62h and a SEN55 at 69h, both silent
    /// until the supply switches on.
    struct Bus {
        powered: bool,
        last: (u8, u16),
        commands: Vec<(u8, u16)>,
        scd4x_ready: bool,
        sen5x_measuring: bool,
    }

    #[derive(Debug)]
    struct Nack;

    impl embedded_hal::i2c::Error for Nack {
        fn kind(&self) -> embedded_hal::i2c::ErrorKind {
            embedded_hal::i2c::ErrorKind::NoAcknowledge(
                embedded_hal::i2c::NoAcknowledgeSource::Address,
            )
        }
    }

    impl ErrorType for Bus {
        type Error = Nack;
    }

    fn framed(words: &[u16]) -> Vec<u8> {
        words
            .iter()
            .flat_map(|word| {
                let [high, low] = word.to_be_bytes();
                [high, low, crc8(&[high, low])]
            })
            .collect()
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            if !self.powered {
                return Err(Nack);
            }
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        let command = u16::from_be_bytes([bytes[0], bytes[1]]);
                        self.last = (address, command);
                        self.commands.push((address, command));
                        if (address, command) == (0x69, 0x0021) {
                            self.sen5x_measuring = true;
                        }
                    }
                    Operation::Read(buffer) => {
                        let reply = match self.last {
                            (0x62, 0x3682) => framed(&[1, 2, 3]),
                            (0x62, 0xe4b8) => framed(&[if self.scd4x_ready { 1 } else { 0 }]),
                            // 800 ppm, 25 degrees Celsius, 37 percent.
                            (0x62, 0xec05) => framed(&[800, 0x6667, 0x5eb9]),
                            (0x69, 0xd014) => {
                                let mut words = vec![
                                    u16::from_be_bytes(*b"SE"),
                                    u16::from_be_bytes(*b"N5"),
                                    u16::from_be_bytes([b'5', 0]),
                                ];
                                words.resize(16, 0);
                                framed(&words)
                            }
                            (0x69, 0x03c4) if self.sen5x_measuring => {
                                framed(&[12, 34, 45, 56, 4_512, 4_700, 1_005, 0x7fff])
                            }
                            _ => framed(&[0xffff; 8]),
                        };
                        buffer.copy_from_slice(&reply[..buffer.len()]);
                    }
                }
            }
            Ok(())
        }
    }

    /// The supply switch, which powers the bus when driven to its active
    /// level (low on the AirQ).
    struct Supply {
        level: Rc<core::cell::Cell<Option<bool>>>,
    }

    impl PinErrorType for Supply {
        type Error = Infallible;
    }

    impl OutputPin for Supply {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.level.set(Some(false));
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.level.set(Some(true));
            Ok(())
        }
    }

    struct Delay;

    impl DelayNs for Delay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    fn bus(powered: bool) -> Bus {
        Bus {
            powered,
            last: (0, 0),
            commands: Vec::new(),
            scd4x_ready: false,
            sen5x_measuring: false,
        }
    }

    #[test]
    fn composes_both_chips_into_one_reading() {
        let level = Rc::new(core::cell::Cell::new(None));
        // The supply is modeled as already on so the bus answers; the
        // implementation still drives it to its active level.
        let mut sensor = block_on(Scd4xSen5xEnvironmentSensorImplementation::initialize(
            Scd4xSen5xBindings::new(
                bus(true),
                Some(Supply {
                    level: Rc::clone(&level),
                }),
                Delay,
            ),
            Scd4xSen5xConfig::new(0x62, 0x69, false),
        ))
        .expect("initialize");
        assert_eq!(level.get(), Some(false), "AirQ supply enabled while low");
        assert_eq!(sensor.sen5x_model(), Model::Sen55);
        assert_eq!(
            sensor.i2c.commands,
            [
                (0x62, 0x3f86),
                (0x62, 0x3682),
                (0x69, 0xd304),
                (0x69, 0xd014),
                (0x62, 0x21b1),
                (0x69, 0x0021),
            ]
        );
        let quantities = sensor.quantities();
        assert_eq!(quantities.len(), 9);
        assert!(!quantities.contains(Quantity::Illuminance));

        // Before the SCD40's first sample: no CO2; the SEN55's NOx index is
        // still warming up.
        let first = block_on(sensor.read()).expect("read");
        assert_eq!(first.co2_ppm, None);
        assert_eq!(first.pm2_5_ng_per_m3, Some(3_400));
        assert_eq!(first.temperature_millicelsius, Some(23_500));
        assert_eq!(first.voc_index_milli, Some(100_500));
        assert_eq!(first.nox_index_milli, None);

        sensor.i2c.scd4x_ready = true;
        let second = block_on(sensor.read()).expect("read");
        assert_eq!(second.co2_ppm, Some(800));
        // The SEN55 measures temperature and humidity in the airflow; its
        // values win over the SCD40's.
        assert_eq!(second.temperature_millicelsius, Some(23_500));
        assert_eq!(second.relative_humidity_millipercent, Some(45_120));

        // Between SCD40 samples the last one is kept.
        sensor.i2c.scd4x_ready = false;
        let third = block_on(sensor.read()).expect("read");
        assert_eq!(third.co2_ppm, Some(800));
    }

    #[test]
    fn a_silent_chip_stops_the_board() {
        let result = block_on(Scd4xSen5xEnvironmentSensorImplementation::<
            Bus,
            Supply,
            Delay,
        >::initialize(
            Scd4xSen5xBindings::new(bus(false), None, Delay),
            Scd4xSen5xConfig::new(0x62, 0x69, true),
        ));
        assert!(matches!(result, Err(Scd4xSen5xInitError::Scd4x(_))));
    }
}

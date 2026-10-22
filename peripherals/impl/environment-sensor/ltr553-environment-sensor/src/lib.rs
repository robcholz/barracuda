//! Environment sensor over a Lite-On LTR-553ALS: ambient illuminance and
//! proximity.
//!
//! Initialization optionally switches on the sensor's supply, checks the
//! chip's part and manufacturer IDs, and starts both converters at 1x gain
//! (1 to 64k lux) with 100 ms integration, so a Board whose sensor does not
//! answer fails to start. A read returns the latest conversions without
//! waiting.
//!
//! On the M5Stack CoreS3 the sensor sits on the camera board, whose 3.3 V
//! rail is the AXP2101's ALDO3. The PMIC stays private: this implementation
//! programs only ALDO3, while the display and the audio codec that share
//! the PMIC program DLDO1 and ALDO1/ALDO2.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_axp2101::{Axp2101, Error as Axp2101Error, Ldo};
use barracuda_driver_ltr553::{AlsGain, Error as ChipError, Ltr553, STARTUP_MS};
use barracuda_peripheral::{
    PeripheralImplementation,
    environment::{EnvironmentReading, EnvironmentSensor, Quantities, Quantity},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

/// AXP2101 address on M5Stack's internal bus.
const PMIC_ADDRESS: u8 = 0x34;
/// M5Unified `Power_Class::begin` CoreS3: ALDO3 at 3.3 V "for camera".
const CAMERA_SUPPLY_MILLIVOLTS: u16 = 3300;

/// Which helper chip supplies the sensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerLayout {
    /// The sensor runs from an always-on rail.
    None,
    /// CoreS3 and StackChan: AXP2101 ALDO3 at 3.3 V.
    CoreS3,
}

/// Board-owned sensor configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ltr553Config {
    address: u8,
    layout: PowerLayout,
}

impl Ltr553Config {
    /// Creates a configuration for one seven-bit I2C address and supply.
    #[must_use]
    pub const fn new(address: u8, layout: PowerLayout) -> Self {
        Self { address, layout }
    }
}

/// Move-only resources consumed by the implementation.
pub struct Ltr553Bindings<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
}

impl<I2C, DELAY> Ltr553Bindings<I2C, DELAY> {
    /// Combines the I2C device view and the delay source.
    #[must_use]
    pub const fn new(i2c: I2C, delay: DELAY) -> Self {
        Self { i2c, delay }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum Ltr553InitError<BusError> {
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress(u8),
    /// The PMIC did not switch the sensor's supply on.
    Supply(Axp2101Error<BusError>),
    /// The sensor did not answer or is another part.
    Sensor(ChipError<BusError>),
}

/// Initialized LTR-553ALS environment sensor.
pub struct Ltr553EnvironmentSensor<I2C> {
    i2c: I2C,
    chip: Ltr553,
}

/// Static factory used by generated Board composition.
pub struct Ltr553EnvironmentSensorImplementation<I2C, DELAY>(PhantomData<fn() -> (I2C, DELAY)>);

impl<I2C, DELAY> PeripheralImplementation for Ltr553EnvironmentSensorImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Ltr553Bindings<I2C, DELAY>;
    type Config = Ltr553Config;
    type Peripheral = Ltr553EnvironmentSensor<I2C>;
    type Error = Ltr553InitError<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let Ltr553Bindings { mut i2c, mut delay } = bindings;
        let chip = Ltr553::new(config.address, AlsGain::X1)
            .ok_or(Ltr553InitError::InvalidAddress(config.address))?;
        if config.layout == PowerLayout::CoreS3 {
            let mut pmic = Axp2101::new(&mut i2c, PMIC_ADDRESS).map_err(Ltr553InitError::Supply)?;
            pmic.set_millivolts(Ldo::Aldo3, CAMERA_SUPPLY_MILLIVOLTS)
                .map_err(Ltr553InitError::Supply)?;
            pmic.set_enabled(Ldo::Aldo3, true)
                .map_err(Ltr553InitError::Supply)?;
            delay.delay_ms(STARTUP_MS);
        }
        chip.start(&mut i2c, &mut delay)
            .map_err(Ltr553InitError::Sensor)?;
        Ok(Ltr553EnvironmentSensor { i2c, chip })
    }
}

impl<I2C> EnvironmentSensor for Ltr553EnvironmentSensor<I2C>
where
    I2C: I2c,
{
    type Error = I2C::Error;

    fn quantities(&self) -> Quantities {
        Quantities::EMPTY
            .with(Quantity::Illuminance)
            .with(Quantity::Proximity)
    }

    async fn read(&mut self) -> Result<EnvironmentReading, Self::Error> {
        let sample = self.chip.read(&mut self.i2c)?;
        Ok(EnvironmentReading {
            illuminance_millilux: Some(sample.illuminance_millilux),
            proximity_counts: Some(u32::from(sample.proximity_counts)),
            ..EnvironmentReading::EMPTY
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use std::vec::Vec;

    use barracuda_peripheral::{
        PeripheralImplementation,
        environment::{EnvironmentSensor, Quantity},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{
        Ltr553Bindings, Ltr553Config, Ltr553EnvironmentSensorImplementation, Ltr553InitError,
        PowerLayout,
    };

    /// The AXP2101 at 34h and the LTR-553ALS at 23h on one bus.
    struct Bus {
        pmic: [u8; 0x100],
        sensor: [u8; 0x20],
        pointer: (u8, u8),
        powered: bool,
        writes: Vec<(u8, u8, u8)>,
    }

    impl Bus {
        fn new(powered: bool) -> Self {
            let mut sensor = [0; 0x20];
            sensor[0x06] = 0x92;
            sensor[0x07] = 0x05;
            // CH1 = 0, CH0 = 1000, proximity 42.
            sensor[0x08..0x0f].copy_from_slice(&[0, 0, 0xe8, 0x03, 0, 42, 0]);
            Self {
                pmic: [0; 0x100],
                sensor,
                pointer: (0, 0),
                powered,
                writes: Vec::new(),
            }
        }
    }

    #[derive(Debug, PartialEq)]
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

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            if address == 0x23 && !self.powered {
                return Err(Nack);
            }
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        self.pointer = (address, bytes[0]);
                        if let [register, value] = bytes[..] {
                            self.writes.push((address, register, value));
                            if address == 0x34 {
                                self.pmic[usize::from(register)] = value;
                                // ALDO3 is bit 2 of the LDO enable register.
                                self.powered |= register == 0x90 && value & 0x04 != 0;
                            } else {
                                self.sensor[usize::from(register - 0x80)] = value;
                            }
                        }
                    }
                    Operation::Read(buffer) => {
                        let (address, register) = self.pointer;
                        let start = usize::from(register);
                        if address == 0x34 {
                            buffer.copy_from_slice(&self.pmic[start..start + buffer.len()]);
                        } else {
                            let start = start - 0x80;
                            buffer.copy_from_slice(&self.sensor[start..start + buffer.len()]);
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
    fn switches_the_cores3_camera_rail_on_and_reads_light_and_proximity() {
        let mut sensor = block_on(Ltr553EnvironmentSensorImplementation::initialize(
            Ltr553Bindings::new(Bus::new(false), Delay),
            Ltr553Config::new(0x23, PowerLayout::CoreS3),
        ))
        .expect("initialize");
        // ALDO3 (94h) set to 3.3 V, code 28, then enabled.
        assert_eq!(sensor.i2c.pmic[0x94], 28);
        assert_eq!(sensor.i2c.pmic[0x90] & 0x04, 0x04);
        let quantities = sensor.quantities();
        assert!(quantities.contains(Quantity::Illuminance));
        assert!(quantities.contains(Quantity::Proximity));
        assert!(!quantities.contains(Quantity::Temperature));
        let reading = block_on(sensor.read()).expect("read");
        // 1.7743 * 1000 = 1774.3 lux.
        assert_eq!(reading.illuminance_millilux, Some(1_774_300));
        assert_eq!(reading.proximity_counts, Some(42));
    }

    #[test]
    fn an_unpowered_sensor_stops_the_board() {
        let result = block_on(Ltr553EnvironmentSensorImplementation::initialize(
            Ltr553Bindings::new(Bus::new(false), Delay),
            Ltr553Config::new(0x23, PowerLayout::None),
        ));
        assert!(matches!(result, Err(Ltr553InitError::Sensor(_))));
    }
}

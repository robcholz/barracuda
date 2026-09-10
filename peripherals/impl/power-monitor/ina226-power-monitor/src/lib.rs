//! INA226 voltage, current, and power monitor implementation.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_ina226::Ina226;
use barracuda_peripheral::{PeripheralImplementation, power::PowerMonitor};
use embedded_hal::i2c::I2c;

pub use barracuda_peripheral::power::PowerMeasurement;

/// Board-owned INA226 configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ina226Config {
    address: u8,
    shunt_micro_ohms: u32,
}

impl Ina226Config {
    /// Creates a monitor configuration with the fitted shunt resistance.
    #[must_use]
    pub const fn new(address: u8, shunt_micro_ohms: u32) -> Self {
        Self {
            address,
            shunt_micro_ohms,
        }
    }
}

/// Move-only resources consumed by the monitor implementation.
pub struct Ina226Bindings<I2C> {
    i2c: I2C,
}

impl<I2C> Ina226Bindings<I2C> {
    /// Wraps an I2C device view.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// INA226 initialization failure.
#[derive(Debug)]
pub enum Ina226InitError<BusError> {
    /// Address or shunt configuration is invalid.
    InvalidConfig,
    /// A register operation failed.
    Bus(BusError),
    /// The device IDs do not identify an INA226.
    UnexpectedDevice {
        /// Value read from the manufacturer-ID register.
        manufacturer: u16,
        /// Value read from the die-ID and revision register.
        die: u16,
    },
}

/// INA226 sampling failure.
#[derive(Debug)]
pub enum Ina226Error<BusError> {
    /// A register operation failed.
    Bus(BusError),
}

/// Initialized INA226 monitor.
pub struct Ina226PowerMonitor<I2C> {
    i2c: I2C,
    chip: Ina226,
    shunt_micro_ohms: u32,
}

/// Static factory used by generated Board composition.
pub struct Ina226PowerMonitorImplementation<I2C>(PhantomData<I2C>);

impl<I2C> PeripheralImplementation for Ina226PowerMonitorImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Ina226Bindings<I2C>;
    type Config = Ina226Config;
    type Peripheral = Ina226PowerMonitor<I2C>;
    type Error = Ina226InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let Some(chip) = Ina226::new(config.address) else {
            return Err(Ina226InitError::InvalidConfig);
        };
        if config.shunt_micro_ohms == 0 {
            return Err(Ina226InitError::InvalidConfig);
        }
        let identity = chip
            .identity(&mut bindings.i2c)
            .map_err(Ina226InitError::Bus)?;
        if !identity.is_ina226() {
            return Err(Ina226InitError::UnexpectedDevice {
                manufacturer: identity.manufacturer,
                die: identity.die,
            });
        }
        Ok(Ina226PowerMonitor {
            i2c: bindings.i2c,
            chip,
            shunt_micro_ohms: config.shunt_micro_ohms,
        })
    }
}

impl<I2C> PowerMonitor for Ina226PowerMonitor<I2C>
where
    I2C: I2c,
{
    type Error = Ina226Error<I2C::Error>;

    /// Samples bus voltage and signed shunt current without altering calibration.
    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        let bus_raw = self
            .chip
            .bus_voltage_raw(&mut self.i2c)
            .map_err(Ina226Error::Bus)?;
        let shunt_raw = self
            .chip
            .shunt_voltage_raw(&mut self.i2c)
            .map_err(Ina226Error::Bus)?;
        let bus_microvolts = u64::from(bus_raw).saturating_mul(1_250);
        let shunt_nanovolts = i64::from(shunt_raw).saturating_mul(2_500);
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

//! BQ27220 battery power-monitor peripheral implementation.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_bq27220::Bq27220;
use barracuda_peripheral::{
    PeripheralImplementation,
    power::{PowerMeasurement, PowerMonitor},
};
use embedded_hal::i2c::I2c;

/// Board-owned BQ27220 configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bq27220Config {
    address: u8,
}

impl Bq27220Config {
    /// Creates a fuel-gauge configuration.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Move-only resources consumed by the monitor implementation.
pub struct Bq27220Bindings<I2C> {
    i2c: I2C,
}

impl<I2C> Bq27220Bindings<I2C> {
    /// Wraps an I2C device view.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// BQ27220 initialization failure.
#[derive(Debug)]
pub enum Bq27220InitError<BusError> {
    /// The configured address is invalid.
    InvalidAddress,
    /// The voltage register could not be read.
    Bus(BusError),
}

/// BQ27220 sampling failure.
#[derive(Debug)]
pub enum Bq27220Error<BusError> {
    /// A register operation failed.
    Bus(BusError),
}

/// Initialized BQ27220 power monitor.
pub struct Bq27220PowerMonitor<I2C> {
    i2c: I2C,
    chip: Bq27220,
}

/// Static factory used by generated Board composition.
pub struct Bq27220PowerMonitorImplementation<I2C>(PhantomData<I2C>);

impl<I2C> PeripheralImplementation for Bq27220PowerMonitorImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Bq27220Bindings<I2C>;
    type Config = Bq27220Config;
    type Peripheral = Bq27220PowerMonitor<I2C>;
    type Error = Bq27220InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let chip = Bq27220::new(config.address).ok_or(Bq27220InitError::InvalidAddress)?;
        chip.voltage_millivolts(&mut bindings.i2c)
            .map_err(Bq27220InitError::Bus)?;
        Ok(Bq27220PowerMonitor {
            i2c: bindings.i2c,
            chip,
        })
    }
}

impl<I2C> PowerMonitor for Bq27220PowerMonitor<I2C>
where
    I2C: I2c,
{
    type Error = Bq27220Error<I2C::Error>;

    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        let sample = self
            .chip
            .measure(&mut self.i2c)
            .map_err(Bq27220Error::Bus)?;
        let bus_microvolts = u64::from(sample.voltage_millivolts).saturating_mul(1_000);
        let current_microamps = i64::from(sample.current_milliamps).saturating_mul(1_000);
        let power_microwatts = i64::try_from(bus_microvolts)
            .unwrap_or(i64::MAX)
            .saturating_mul(current_microamps)
            .checked_div(1_000_000)
            .unwrap_or_default();
        Ok(PowerMeasurement {
            bus_microvolts,
            shunt_nanovolts: 0,
            current_microamps,
            power_microwatts,
        })
    }
}

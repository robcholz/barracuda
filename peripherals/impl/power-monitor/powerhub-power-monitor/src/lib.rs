//! The M5Stack PowerHub's battery, read through its power co-processor.
//!
//! Start-up enables the co-processor's metering (register 05h), as
//! M5Unified `Power_Class::begin` does. A measurement reads the battery
//! voltage (30h, millivolts) and current (32h, milliamps, discharge
//! positive); the reported current is positive while the battery charges,
//! as M5Unified `getBatteryCurrent` reports it. The co-processor does not
//! expose the shunt voltage, so the measurement carries none.
//!
//! The charge estimate is M5Unified's for this Board: the per-cell voltage
//! (half the 2S pack's) mapped linearly from 3.3 V (0 %) over 0.8 V, and
//! charging while more than 10 mA flows in. The PowerHub without its kit
//! battery reads no pack; below 5 V (two cells at their 2.5 V cut-off) the
//! monitor reports no battery.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_m5powerhub::M5PowerHub;
use barracuda_peripheral::{
    PeripheralImplementation,
    power::{BatteryState, PowerMeasurement, PowerMonitor},
};
use embedded_hal::i2c::I2c;

/// Below this pack voltage no battery is attached.
const NO_BATTERY_MILLIVOLTS: u16 = 5_000;
/// M5Unified `getBatteryLevel`: 0 % at 3.3 V per cell.
const EMPTY_CELL_MILLIVOLTS: i32 = 3_300;
/// M5Unified `getBatteryLevel`: 100 % 0.8 V above empty (`4150 - 3350`).
const CELL_SPAN_MILLIVOLTS: i32 = 800;
/// M5Unified `isCharging`: charging above 10 mA into the battery.
const CHARGING_MILLIAMPS: i32 = 10;

/// Board-owned co-processor address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerHubPowerMonitorConfig {
    address: u8,
}

impl PowerHubPowerMonitorConfig {
    /// Creates configuration for the co-processor at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Monitor failure.
#[derive(Debug, PartialEq, Eq)]
pub enum PowerHubPowerMonitorError<E> {
    /// The address is not a seven-bit address.
    InvalidAddress(u8),
    /// The co-processor could not be read or written.
    Bus(E),
}

/// Initialized battery monitor.
pub struct PowerHubPowerMonitor<I2C> {
    i2c: I2C,
    hub: M5PowerHub,
}

/// Static factory used by generated Board composition.
pub struct PowerHubPowerMonitorImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for PowerHubPowerMonitorImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = I2C;
    type Config = PowerHubPowerMonitorConfig;
    type Peripheral = PowerHubPowerMonitor<I2C>;
    type Error = PowerHubPowerMonitorError<I2C::Error>;

    async fn initialize(i2c: I2C, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let hub = M5PowerHub::new(config.address)
            .ok_or(PowerHubPowerMonitorError::InvalidAddress(config.address))?;
        let mut monitor = PowerHubPowerMonitor { i2c, hub };
        monitor
            .hub
            .enable_metering(&mut monitor.i2c)
            .map_err(PowerHubPowerMonitorError::Bus)?;
        Ok(monitor)
    }
}

impl<I2C: I2c> PowerHubPowerMonitor<I2C> {
    fn sample(&mut self) -> Result<(u16, i32), PowerHubPowerMonitorError<I2C::Error>> {
        let millivolts = self
            .hub
            .battery_millivolts(&mut self.i2c)
            .map_err(PowerHubPowerMonitorError::Bus)?;
        let milliamps = self
            .hub
            .battery_charge_milliamps(&mut self.i2c)
            .map_err(PowerHubPowerMonitorError::Bus)?;
        Ok((millivolts, milliamps))
    }
}

impl<I2C: I2c> PowerMonitor for PowerHubPowerMonitor<I2C> {
    type Error = PowerHubPowerMonitorError<I2C::Error>;

    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        let (millivolts, milliamps) = self.sample()?;
        let bus_microvolts = u64::from(millivolts).saturating_mul(1_000);
        let current_microamps = i64::from(milliamps).saturating_mul(1_000);
        let power_microwatts = i64::from(millivolts).saturating_mul(i64::from(milliamps));
        Ok(PowerMeasurement {
            bus_microvolts,
            shunt_nanovolts: 0,
            current_microamps,
            power_microwatts,
        })
    }

    fn battery(&mut self) -> Result<Option<BatteryState>, Self::Error> {
        let (millivolts, milliamps) = self.sample()?;
        if millivolts < NO_BATTERY_MILLIVOLTS {
            return Ok(None);
        }
        let cell = i32::from(millivolts / 2);
        let level = cell
            .saturating_sub(EMPTY_CELL_MILLIVOLTS)
            .saturating_mul(100)
            .checked_div(CELL_SPAN_MILLIVOLTS)
            .unwrap_or(0)
            .clamp(0, 100);
        Ok(Some(BatteryState {
            percent: u8::try_from(level).unwrap_or(0),
            charging: milliamps > CHARGING_MILLIAMPS,
        }))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_peripheral::{PeripheralImplementation, power::PowerMonitor};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{PowerHubPowerMonitorConfig, PowerHubPowerMonitorImplementation};

    struct Registers([u8; 256]);

    impl ErrorType for Registers {
        type Error = Infallible;
    }

    impl I2c for Registers {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        register = usize::from(bytes[0]);
                        for (offset, value) in bytes[1..].iter().enumerate() {
                            self.0[register + offset] = *value;
                        }
                    }
                    Operation::Read(values) => {
                        for (offset, value) in values.iter_mut().enumerate() {
                            *value = self.0[register + offset];
                        }
                    }
                }
            }
            Ok(())
        }
    }

    fn monitor(millivolts: u16, raw_milliamps: i16) -> super::PowerHubPowerMonitor<Registers> {
        let mut registers = Registers([0; 256]);
        registers.0[0x30..0x32].copy_from_slice(&millivolts.to_le_bytes());
        registers.0[0x32..0x34].copy_from_slice(&raw_milliamps.to_le_bytes());
        block_on(PowerHubPowerMonitorImplementation::initialize(
            registers,
            PowerHubPowerMonitorConfig::new(0x50),
        ))
        .expect("start")
    }

    #[test]
    fn enables_metering_and_measures_charge_current_positive() {
        let mut monitor = monitor(8_000, -500);
        assert_eq!(monitor.i2c.0[0x05], 1);
        let measurement = monitor.measure().expect("measure");
        assert_eq!(measurement.bus_microvolts, 8_000_000);
        assert_eq!(measurement.current_microamps, 500_000);
        assert_eq!(measurement.power_microwatts, 4_000_000);
    }

    #[test]
    fn estimates_the_2s_charge_as_m5unified_does() {
        let state = monitor(7_400, 200)
            .battery()
            .expect("battery")
            .expect("pack");
        // 3.7 V per cell: (3700 - 3300) * 100 / 800 = 50 %; discharging.
        assert_eq!((state.percent, state.charging), (50, false));
        let state = monitor(8_400, -300)
            .battery()
            .expect("battery")
            .expect("pack");
        assert_eq!((state.percent, state.charging), (100, true));
    }

    #[test]
    fn reports_no_battery_without_a_pack() {
        assert_eq!(monitor(0, 0).battery().expect("battery"), None);
    }
}

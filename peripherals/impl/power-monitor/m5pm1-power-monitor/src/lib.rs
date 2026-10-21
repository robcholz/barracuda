//! Battery monitor for the M5Stack M5PM1 power controller.
//!
//! The M5PM1 measures the battery voltage and reports which supplies are
//! present (M5Unified `M5PM1_Class::getBatteryVoltage` and
//! `getPowerSource`). The charge follows M5Unified's linear estimate
//! (3.3 V empty, 4.1 V full). Whether the battery charges comes from the
//! charger's active-low status line when the Board routes it to an M5PM1
//! pin (M5Unified `Power_Class::isCharging`: StickS3 G0, StopWatch and
//! StampS3-Bat G2, PaperDIY G3); otherwise it is estimated from VIN.
//!
//! # Shared ownership
//!
//! On several Boards a display implementation drives M5PM1 rails and pins
//! through its own view of the bus. This implementation reads only the
//! battery-voltage and power-source registers, configures only its status
//! pin, and repeats the idempotent housekeeping writes (I2C sleep and
//! watchdog off) that the display implementations also make.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_m5pm1::{Error as M5pm1Error, M5pm1, Pull};
use barracuda_peripheral::{
    PeripheralImplementation,
    power::{BatteryState, PowerMeasurement, PowerMonitor},
};
use embedded_hal::i2c::I2c;

/// M5Unified `Power_Class::getBatteryLevel`.
const EMPTY_MILLIVOLTS: u16 = 3_300;
const SPAN_MILLIVOLTS: u16 = 800;
/// Below this, a cell on external power is taken to be charging.
const FULL_MILLIVOLTS: u16 = EMPTY_MILLIVOLTS.saturating_add(SPAN_MILLIVOLTS);
/// The M5PM1 reports no battery voltage below this: no cell is connected.
const ABSENT_MILLIVOLTS: u16 = 1_000;

/// Board-owned address and charge-status pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct M5pm1PowerMonitorConfig {
    address: u8,
    charge_status: Option<u8>,
}

impl M5pm1PowerMonitorConfig {
    /// Creates a configuration; `charge_status` is the M5PM1 pin the
    /// charger's active-low status drives, if any.
    #[must_use]
    pub const fn new(address: u8, charge_status: Option<u8>) -> Self {
        Self {
            address,
            charge_status,
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct M5pm1PowerMonitorBindings<I2C> {
    i2c: I2C,
}

impl<I2C> M5pm1PowerMonitorBindings<I2C> {
    /// Wraps this implementation's own view of the M5PM1's bus.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// Initialized M5PM1 battery monitor.
pub struct M5pm1PowerMonitor<I2C> {
    i2c: I2C,
    config: M5pm1PowerMonitorConfig,
}

/// Static factory used by generated Board composition.
pub struct M5pm1PowerMonitorImplementation<I2C>(PhantomData<I2C>);

impl<I2C> PeripheralImplementation for M5pm1PowerMonitorImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = M5pm1PowerMonitorBindings<I2C>;
    type Config = M5pm1PowerMonitorConfig;
    type Peripheral = M5pm1PowerMonitor<I2C>;
    type Error = M5pm1Error<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let mut i2c = bindings.i2c;
        {
            let mut pm1 = M5pm1::new(&mut i2c, config.address)?;
            pm1.disable_i2c_sleep()?;
            pm1.disable_watchdog()?;
            if let Some(pin) = config.charge_status {
                pm1.configure_input(pin, Pull::Up)?;
            }
            pm1.battery_millivolts()?;
        }
        Ok(M5pm1PowerMonitor { i2c, config })
    }
}

impl<I2C: I2c> M5pm1PowerMonitor<I2C> {
    fn millivolts(&mut self) -> Result<u16, M5pm1Error<I2C::Error>> {
        M5pm1::new(&mut self.i2c, self.config.address)?.battery_millivolts()
    }
}

impl<I2C: I2c> PowerMonitor for M5pm1PowerMonitor<I2C> {
    type Error = M5pm1Error<I2C::Error>;

    /// Reads the battery voltage; the M5PM1 measures no current, so current
    /// and power read zero.
    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        let millivolts = self.millivolts()?;
        Ok(PowerMeasurement {
            bus_microvolts: u64::from(millivolts).saturating_mul(1_000),
            shunt_nanovolts: 0,
            current_microamps: 0,
            power_microwatts: 0,
        })
    }

    fn battery(&mut self) -> Result<Option<BatteryState>, Self::Error> {
        let mut pm1 = M5pm1::new(&mut self.i2c, self.config.address)?;
        let millivolts = pm1.battery_millivolts()?;
        if millivolts < ABSENT_MILLIVOLTS {
            return Ok(None);
        }
        let charging = match self.config.charge_status {
            Some(pin) => !pm1.input_is_high(pin)?,
            None => pm1.power_source()?.vin() && millivolts < FULL_MILLIVOLTS,
        };
        Ok(Some(BatteryState {
            percent: voltage_percent(millivolts),
            charging,
        }))
    }
}

/// M5Unified's linear voltage-to-charge estimate, clamped to 0..=100.
fn voltage_percent(millivolts: u16) -> u8 {
    let percent = u32::from(millivolts.saturating_sub(EMPTY_MILLIVOLTS))
        .saturating_mul(100)
        .checked_div(u32::from(SPAN_MILLIVOLTS))
        .unwrap_or_default()
        .min(100);
    u8::try_from(percent).unwrap_or(100)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use barracuda_peripheral::{
        PeripheralImplementation,
        power::{BatteryState, PowerMonitor},
    };
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{
        M5pm1PowerMonitorBindings, M5pm1PowerMonitorConfig, M5pm1PowerMonitorImplementation,
    };

    struct Pm1 {
        registers: [u8; 0x40],
        writes: Vec<u8>,
    }

    impl Pm1 {
        fn new(millivolts: u16) -> Self {
            let mut registers = [0; 0x40];
            registers[0x22..0x24].copy_from_slice(&millivolts.to_le_bytes());
            // G0 is the display's output, high; G2 reads low.
            registers[0x10] = 0b0000_0001;
            registers[0x12] = 0b0000_0001;
            Self {
                registers,
                writes: Vec::new(),
            }
        }
    }

    impl ErrorType for Pm1 {
        type Error = Infallible;
    }

    impl I2c for Pm1 {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x6e);
            let mut pointer = 0;
            for operation in operations {
                match operation {
                    Operation::Write([register]) => pointer = usize::from(*register),
                    Operation::Write([register, value]) => {
                        self.registers[usize::from(*register)] = *value;
                        self.writes.push(*register);
                    }
                    Operation::Read(bytes) => {
                        for (offset, value) in bytes.iter_mut().enumerate() {
                            *value = self.registers[pointer + offset];
                        }
                    }
                    Operation::Write(_) => {}
                }
            }
            Ok(())
        }
    }

    #[test]
    fn reads_the_status_pin_and_keeps_other_pins() {
        // StopWatch and StampS3-Bat: CHG_STAT on G2, low while charging.
        let mut monitor = block_on(M5pm1PowerMonitorImplementation::initialize(
            M5pm1PowerMonitorBindings::new(Pm1::new(3_900)),
            M5pm1PowerMonitorConfig::new(0x6e, Some(2)),
        ))
        .expect("initialize");
        assert_eq!(
            monitor.i2c.registers[0x10], 0b0000_0001,
            "G0 stays an output"
        );
        assert_eq!(monitor.i2c.registers[0x14], 0b0001_0000, "G2 pulled up");
        let sample = monitor.measure().expect("measure");
        assert_eq!(sample.bus_microvolts, 3_900_000);
        assert_eq!(
            monitor.battery().expect("battery"),
            Some(BatteryState {
                percent: 75,
                charging: true,
            })
        );
        monitor.i2c.registers[0x12] = 0b0000_0101;
        assert!(
            !monitor
                .battery()
                .expect("battery")
                .expect("present")
                .charging
        );
        assert!(
            monitor
                .i2c
                .writes
                .iter()
                .all(|register| [0x09, 0x0a, 0x10, 0x14, 0x16].contains(register))
        );
    }

    #[test]
    fn estimates_charging_from_vin_and_reports_a_missing_cell() {
        let mut monitor = block_on(M5pm1PowerMonitorImplementation::initialize(
            M5pm1PowerMonitorBindings::new(Pm1::new(4_000)),
            M5pm1PowerMonitorConfig::new(0x6e, None),
        ))
        .expect("initialize");
        assert!(
            !monitor
                .battery()
                .expect("battery")
                .expect("present")
                .charging
        );
        monitor.i2c.registers[0x04] = 0b001;
        assert!(
            monitor
                .battery()
                .expect("battery")
                .expect("present")
                .charging
        );
        monitor.i2c.registers[0x22..0x24].copy_from_slice(&4_150_u16.to_le_bytes());
        let full = monitor.battery().expect("battery").expect("present");
        assert!(!full.charging);
        assert_eq!(full.percent, 100);
        monitor.i2c.registers[0x22..0x24].copy_from_slice(&0_u16.to_le_bytes());
        assert_eq!(monitor.battery().expect("battery"), None);
    }
}

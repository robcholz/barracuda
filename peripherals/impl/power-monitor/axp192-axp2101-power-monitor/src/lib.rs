//! Battery monitor for the X-Powers AXP192 and AXP2101 PMICs.
//!
//! The M5Stack Core2 carries an AXP192 on its first revision and an AXP2101
//! on v1.1 at the same address, so the implementation reads the chip ID at
//! start, as M5Unified `Power_Class::begin` does.
//!
//! # Shared ownership
//!
//! On these Boards the PMIC also powers, resets, and lights the display, and
//! `m5stack-pmic-display` drives those regulators. The two implementations
//! hold separate views of the shared I2C bus and split the PMIC by register:
//! this one reads and writes only the measurement registers the chip drivers
//! document (status, ADC enable, battery ADC, fuel gauge, battery
//! detection), and never a regulator, voltage, or GPIO register. Neither
//! read-modify-writes a register the other owns, so their bus transfers may
//! interleave freely.
//!
//! # Readings
//!
//! - AXP192: battery voltage and the difference of the charge and discharge
//!   currents, positive while charging; the charge in percent follows
//!   M5Unified `getBatteryLevel` (3.3 V empty, 4.1 V full).
//! - AXP2101: battery voltage and the fuel gauge's percentage. The chip has
//!   no battery-current ADC: current and power read zero unless the Board
//!   names a current sense. On the Core2 v1.1, channel 1 of an INA3221 at
//!   `40h` measures the battery current across a 10 mΩ shunt (M5Unified
//!   `Power_Class::getBatteryCurrent`); the implementation only reads it.
//!
//! A Board running without a battery reports none.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_axp192::{Axp192, Error as Axp192Error};
use barracuda_driver_axp2101::{Axp2101, ChargeDirection, Error as Axp2101Error};
use barracuda_driver_ina3221::{
    DEFAULT_ADDRESS as INA3221_ADDRESS, Error as Ina3221Error, Ina3221,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    power::{BatteryState, PowerMeasurement, PowerMonitor},
};
use embedded_hal::i2c::I2c;

/// M5Unified `Power_Class::getBatteryLevel`: 0 % at 3300 mV, 100 % at
/// 3300 mV + 800 mV and above.
const EMPTY_MILLIVOLTS: u32 = 3_300;
const SPAN_MILLIVOLTS: u32 = 800;
/// The INA3221 channel and shunt that sense the Core2 v1.1 battery current
/// (M5Unified: `Ina3221[0].getCurrent(0)` with `_shunt_res` 10 mΩ).
const BATTERY_CURRENT_CHANNEL: u8 = 0;
const BATTERY_SHUNT_MILLIOHMS: i64 = 10;

/// Board-owned PMIC address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxpPowerMonitorConfig {
    address: u8,
    ina3221_battery_current: bool,
}

impl AxpPowerMonitorConfig {
    /// Creates a configuration for the PMIC at `address`, optionally with
    /// the INA3221 battery-current sense of the Core2 v1.1.
    #[must_use]
    pub const fn new(address: u8, ina3221_battery_current: bool) -> Self {
        Self {
            address,
            ina3221_battery_current,
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct AxpPowerMonitorBindings<I2C> {
    i2c: I2C,
}

impl<I2C> AxpPowerMonitorBindings<I2C> {
    /// Wraps this implementation's own view of the PMIC's bus.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// Which PMIC answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pmic {
    /// X-Powers AXP192.
    Axp192,
    /// X-Powers AXP2101.
    Axp2101,
}

/// Initialization failure.
#[derive(Debug)]
pub enum AxpPowerMonitorInitError<BusError> {
    /// No AXP192 or AXP2101 answered with its chip ID.
    UnsupportedPmic,
    /// An AXP192 register operation failed.
    Axp192(Axp192Error<BusError>),
    /// An AXP2101 register operation failed.
    Axp2101(Axp2101Error<BusError>),
}

/// Sampling failure.
#[derive(Debug)]
pub enum AxpPowerMonitorError<BusError> {
    /// An AXP192 register operation failed.
    Axp192(Axp192Error<BusError>),
    /// An AXP2101 register operation failed.
    Axp2101(Axp2101Error<BusError>),
    /// The INA3221 current sense failed.
    Ina3221(Ina3221Error<BusError>),
}

/// Initialized PMIC battery monitor.
pub struct AxpPowerMonitor<I2C> {
    i2c: I2C,
    address: u8,
    pmic: Pmic,
    current_sense: Option<Ina3221>,
}

/// Static factory used by generated Board composition.
pub struct AxpPowerMonitorImplementation<I2C>(PhantomData<I2C>);

impl<I2C> PeripheralImplementation for AxpPowerMonitorImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = AxpPowerMonitorBindings<I2C>;
    type Config = AxpPowerMonitorConfig;
    type Peripheral = AxpPowerMonitor<I2C>;
    type Error = AxpPowerMonitorInitError<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let mut i2c = bindings.i2c;
        let address = config.address;
        // Both PMICs keep their ID in REG03H: 03h on an AXP192, 4Ah on an
        // AXP2101.
        let mut axp192 =
            Axp192::new(&mut i2c, address).map_err(AxpPowerMonitorInitError::Axp192)?;
        let pmic = if axp192
            .is_present()
            .map_err(AxpPowerMonitorInitError::Axp192)?
        {
            axp192
                .enable_battery_adc()
                .map_err(AxpPowerMonitorInitError::Axp192)?;
            Pmic::Axp192
        } else {
            let mut axp2101 =
                Axp2101::new(&mut i2c, address).map_err(AxpPowerMonitorInitError::Axp2101)?;
            if !axp2101
                .is_present()
                .map_err(AxpPowerMonitorInitError::Axp2101)?
            {
                return Err(AxpPowerMonitorInitError::UnsupportedPmic);
            }
            axp2101
                .enable_battery_measurement()
                .map_err(AxpPowerMonitorInitError::Axp2101)?;
            Pmic::Axp2101
        };
        // The Core2 v1.0 has no INA3221; an unanswered probe leaves none.
        let current_sense = Ina3221::new(INA3221_ADDRESS).filter(|sense| {
            pmic == Pmic::Axp2101
                && config.ina3221_battery_current
                && sense.is_present(&mut i2c).unwrap_or(false)
        });
        Ok(AxpPowerMonitor {
            i2c,
            address,
            pmic,
            current_sense,
        })
    }
}

impl<I2C> AxpPowerMonitor<I2C> {
    /// Returns the PMIC that answered at initialization.
    #[must_use]
    pub const fn pmic(&self) -> Pmic {
        self.pmic
    }

    /// Returns whether an INA3221 senses the battery current.
    #[must_use]
    pub const fn has_current_sense(&self) -> bool {
        self.current_sense.is_some()
    }
}

impl<I2C: I2c> PowerMonitor for AxpPowerMonitor<I2C> {
    type Error = AxpPowerMonitorError<I2C::Error>;

    /// Reads the battery voltage and, where the Board senses it, its current.
    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        let mut shunt_nanovolts = 0;
        let (microvolts, current_microamps) = match self.pmic {
            Pmic::Axp192 => {
                let sample = Axp192::new(&mut self.i2c, self.address)
                    .and_then(|mut pmic| pmic.battery_measurement())
                    .map_err(AxpPowerMonitorError::Axp192)?;
                (
                    u64::from(sample.microvolts),
                    i64::from(sample.charge_microamps)
                        .saturating_sub(i64::from(sample.discharge_microamps)),
                )
            }
            Pmic::Axp2101 => {
                let millivolts = Axp2101::new(&mut self.i2c, self.address)
                    .and_then(|mut pmic| pmic.battery_millivolts())
                    .map_err(AxpPowerMonitorError::Axp2101)?;
                let current_microamps = match self.current_sense {
                    Some(sense) => {
                        let shunt = sense
                            .read_channel(&mut self.i2c, BATTERY_CURRENT_CHANNEL)
                            .map_err(AxpPowerMonitorError::Ina3221)?
                            .shunt_microvolts;
                        shunt_nanovolts = i64::from(shunt).saturating_mul(1_000);
                        // µV / mΩ = mA.
                        i64::from(shunt)
                            .saturating_mul(1_000)
                            .checked_div(BATTERY_SHUNT_MILLIOHMS)
                            .unwrap_or_default()
                    }
                    None => 0,
                };
                (
                    u64::from(millivolts).saturating_mul(1_000),
                    current_microamps,
                )
            }
        };
        Ok(PowerMeasurement {
            bus_microvolts: microvolts,
            shunt_nanovolts,
            current_microamps,
            power_microwatts: i64::try_from(microvolts)
                .unwrap_or(i64::MAX)
                .saturating_mul(current_microamps)
                .checked_div(1_000_000)
                .unwrap_or_default(),
        })
    }

    fn battery(&mut self) -> Result<Option<BatteryState>, Self::Error> {
        match self.pmic {
            Pmic::Axp192 => {
                let mut pmic = Axp192::new(&mut self.i2c, self.address)
                    .map_err(AxpPowerMonitorError::Axp192)?;
                let status = pmic
                    .battery_status()
                    .map_err(AxpPowerMonitorError::Axp192)?;
                if !status.battery_present {
                    return Ok(None);
                }
                let sample = pmic
                    .battery_measurement()
                    .map_err(AxpPowerMonitorError::Axp192)?;
                Ok(Some(BatteryState {
                    percent: voltage_percent(sample.microvolts / 1_000),
                    charging: status.charging,
                }))
            }
            Pmic::Axp2101 => {
                let mut pmic = Axp2101::new(&mut self.i2c, self.address)
                    .map_err(AxpPowerMonitorError::Axp2101)?;
                let status = pmic
                    .battery_status()
                    .map_err(AxpPowerMonitorError::Axp2101)?;
                if !status.battery_present {
                    return Ok(None);
                }
                let percent = pmic
                    .battery_percent()
                    .map_err(AxpPowerMonitorError::Axp2101)?;
                Ok(Some(BatteryState {
                    percent: percent.min(100),
                    charging: status.direction == ChargeDirection::Charging,
                }))
            }
        }
    }
}

/// M5Unified's linear voltage-to-charge estimate, clamped to 0..=100.
fn voltage_percent(millivolts: u32) -> u8 {
    let percent = millivolts
        .saturating_sub(EMPTY_MILLIVOLTS)
        .saturating_mul(100)
        .checked_div(SPAN_MILLIVOLTS)
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
        AxpPowerMonitorBindings, AxpPowerMonitorConfig, AxpPowerMonitorImplementation,
        AxpPowerMonitorInitError, Pmic, voltage_percent,
    };

    /// Registers the display owns: output control, voltages, GPIO, LDOs.
    const DISPLAY_REGISTERS: [u8; 9] = [0x12, 0x23, 0x26, 0x27, 0x28, 0x90, 0x92, 0x95, 0x96];

    struct Pmic34 {
        registers: [u8; 0x100],
        writes: Vec<u8>,
    }

    impl ErrorType for Pmic34 {
        type Error = Infallible;
    }

    impl I2c for Pmic34 {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x34);
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

    fn axp192() -> Pmic34 {
        let mut registers = [0; 0x100];
        registers[0x03] = 0x03;
        // Charging from VBUS with a battery present.
        registers[0x00] = 0b0010_0100;
        registers[0x01] = 0b0010_0000;
        // 3.9006 V (3546 x 1.1 mV), 100 mA in, 20 mA out.
        registers[0x78] = 0xdd;
        registers[0x79] = 0x0a;
        registers[0x7a] = 0x06;
        registers[0x7b] = 0x08;
        registers[0x7c] = 0x01;
        registers[0x7d] = 0x08;
        Pmic34 {
            registers,
            writes: Vec::new(),
        }
    }

    #[test]
    fn reads_an_axp192_battery_and_writes_only_measurement_registers() {
        let mut monitor = block_on(AxpPowerMonitorImplementation::initialize(
            AxpPowerMonitorBindings::new(axp192()),
            AxpPowerMonitorConfig::new(0x34, false),
        ))
        .expect("initialize");
        assert_eq!(monitor.pmic(), Pmic::Axp192);
        let sample = monitor.measure().expect("measure");
        assert_eq!(sample.bus_microvolts, 3_900_600);
        assert_eq!(sample.current_microamps, 80_000);
        assert_eq!(sample.power_microwatts, 312_048);
        assert_eq!(
            monitor.battery().expect("battery"),
            Some(BatteryState {
                percent: 75,
                charging: true,
            })
        );
        assert_eq!(monitor.i2c.writes, [0x82]);
        assert!(
            monitor
                .i2c
                .writes
                .iter()
                .all(|register| !DISPLAY_REGISTERS.contains(register))
        );
    }

    #[test]
    fn reads_an_axp2101_fuel_gauge() {
        let mut registers = [0; 0x100];
        registers[0x03] = 0x4a;
        registers[0x00] = 0b0000_1000;
        registers[0x01] = 0b0100_0000;
        registers[0x34] = 0x0f;
        registers[0x35] = 0x00;
        registers[0xa4] = 64;
        let mut monitor = block_on(AxpPowerMonitorImplementation::initialize(
            AxpPowerMonitorBindings::new(Pmic34 {
                registers,
                writes: Vec::new(),
            }),
            AxpPowerMonitorConfig::new(0x34, false),
        ))
        .expect("initialize");
        assert_eq!(monitor.pmic(), Pmic::Axp2101);
        let sample = monitor.measure().expect("measure");
        assert_eq!(sample.bus_microvolts, 3_840_000);
        assert_eq!(sample.current_microamps, 0);
        assert_eq!(
            monitor.battery().expect("battery"),
            Some(BatteryState {
                percent: 64,
                charging: false,
            })
        );
        assert_eq!(monitor.i2c.writes, [0x30, 0x68]);
        monitor.i2c.registers[0x00] = 0;
        assert_eq!(monitor.battery().expect("battery"), None);
    }

    #[test]
    fn rejects_another_chip_and_clamps_the_voltage_estimate() {
        let mut registers = [0; 0x100];
        registers[0x03] = 0x51;
        let result = block_on(AxpPowerMonitorImplementation::initialize(
            AxpPowerMonitorBindings::new(Pmic34 {
                registers,
                writes: Vec::new(),
            }),
            AxpPowerMonitorConfig::new(0x34, false),
        ));
        assert!(matches!(
            result,
            Err(AxpPowerMonitorInitError::UnsupportedPmic)
        ));
        assert_eq!(voltage_percent(3_000), 0);
        assert_eq!(voltage_percent(3_700), 50);
        assert_eq!(voltage_percent(4_200), 100);
    }

    /// The Core2 v1.1: an AXP2101 at 34h and an INA3221 at 40h.
    struct Core2V11 {
        pmic: Pmic34,
    }

    impl ErrorType for Core2V11 {
        type Error = Infallible;
    }

    impl I2c for Core2V11 {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            if address == 0x34 {
                return self.pmic.transaction(address, operations);
            }
            assert_eq!(address, 0x40);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => register = bytes[0],
                    Operation::Read(bytes) => {
                        let value: u16 = match register {
                            // -2.0 mV across 10 mOhm: 200 mA out of the battery.
                            0x01 => (-50_i16 << 3).cast_unsigned(),
                            0xfe => 0x5449,
                            0xff => 0x3220,
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
    fn takes_the_core2_v1_1_battery_current_from_its_ina3221() {
        let mut registers = [0; 0x100];
        registers[0x03] = 0x4a;
        registers[0x34] = 0x0f;
        registers[0x35] = 0xa0;
        let mut monitor = block_on(AxpPowerMonitorImplementation::initialize(
            AxpPowerMonitorBindings::new(Core2V11 {
                pmic: Pmic34 {
                    registers,
                    writes: Vec::new(),
                },
            }),
            AxpPowerMonitorConfig::new(0x34, true),
        ))
        .expect("initialize");
        assert!(monitor.has_current_sense());
        let sample = monitor.measure().expect("measure");
        assert_eq!(sample.bus_microvolts, 4_000_000);
        assert_eq!(sample.shunt_nanovolts, -2_000_000);
        assert_eq!(sample.current_microamps, -200_000);
        assert_eq!(sample.power_microwatts, -800_000);
    }
}

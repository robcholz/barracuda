//! Battery power monitor that samples a battery through a resistor divider
//! on an ADC pin.
//!
//! Many M5Stack and Unexpected Maker Boards have no fuel gauge: the battery
//! reaches an ADC pin through two resistors, and firmware multiplies the pin
//! voltage by the divider's ratio (M5Unified `Power_Class::getBatteryVoltage`
//! for `pmic_adc`, TinyPICO `GetBatteryVoltage`). The charge percentage is a
//! straight line between an empty and a full cell voltage, and a charger's
//! status output, when the Board wires one to a GPIO, says whether it charges.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::AnalogVoltageInput;
use barracuda_peripheral::{
    PeripheralImplementation,
    power::{BatteryState, PowerMeasurement, PowerMonitor},
};
use embedded_hal::digital::InputPin;

/// Conversions averaged into one battery reading, which smooths the
/// converter's noise.
const SAMPLES: u32 = 4;

/// The divider and cell voltages of one Board's battery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdcBatteryConfig {
    divider_numerator: u32,
    divider_denominator: u32,
    empty_millivolts: u32,
    full_millivolts: u32,
    charging_level_high: bool,
}

impl AdcBatteryConfig {
    /// Creates a configuration: battery voltage is the pin voltage times
    /// `divider_numerator / divider_denominator`, 0 % at `empty_millivolts`
    /// and 100 % at `full_millivolts`; the charger charges while its status
    /// input reads high when `charging_level_high` is set, low otherwise.
    #[must_use]
    pub const fn new(
        divider_numerator: u32,
        divider_denominator: u32,
        empty_millivolts: u32,
        full_millivolts: u32,
        charging_level_high: bool,
    ) -> Self {
        Self {
            divider_numerator,
            divider_denominator,
            empty_millivolts,
            full_millivolts,
            charging_level_high,
        }
    }

    /// Converts one pin voltage into the battery voltage.
    #[must_use]
    pub const fn battery_millivolts(self, pin_millivolts: u32) -> u32 {
        let scaled =
            pin_millivolts as u64 * self.divider_numerator as u64 / self.divider_denominator as u64;
        if scaled > u32::MAX as u64 {
            u32::MAX
        } else {
            scaled as u32
        }
    }

    /// Converts one battery voltage into a charge percentage.
    #[must_use]
    pub const fn percent(self, battery_millivolts: u32) -> u8 {
        if battery_millivolts <= self.empty_millivolts {
            return 0;
        }
        if battery_millivolts >= self.full_millivolts {
            return 100;
        }
        let span = self.full_millivolts - self.empty_millivolts;
        ((battery_millivolts - self.empty_millivolts) * 100 / span) as u8
    }

    const fn is_valid(self) -> bool {
        self.divider_numerator != 0
            && self.divider_denominator != 0
            && self.empty_millivolts < self.full_millivolts
    }
}

/// Move-only ADC channel and optional charger status input.
pub struct AdcBatteryBindings<SENSE, CHARGING> {
    sense: SENSE,
    charging: Option<CHARGING>,
}

impl<SENSE, CHARGING> AdcBatteryBindings<SENSE, CHARGING> {
    /// Combines the divider's ADC channel and the charger's status input.
    #[must_use]
    pub const fn new(sense: SENSE, charging: Option<CHARGING>) -> Self {
        Self { sense, charging }
    }
}

/// Configuration rejected before the monitor starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdcBatteryInitError {
    /// The divider has a zero term or the full voltage is not above empty.
    InvalidConfig,
}

/// Sampling failure of the ADC channel or the charger status input.
#[derive(Debug)]
pub enum AdcBatteryError<SenseError, ChargingError> {
    /// The ADC conversion failed.
    Sense(SenseError),
    /// The charger status input could not be read.
    Charging(ChargingError),
}

/// Battery monitor over an ADC channel behind a resistor divider.
pub struct AdcBatteryPowerMonitor<SENSE, CHARGING> {
    sense: SENSE,
    charging: Option<CHARGING>,
    config: AdcBatteryConfig,
}

impl<SENSE, CHARGING> AdcBatteryPowerMonitor<SENSE, CHARGING>
where
    SENSE: AnalogVoltageInput,
{
    fn battery_millivolts(&mut self) -> Result<u32, SENSE::Error> {
        let mut total = 0_u32;
        for _ in 0..SAMPLES {
            total = total.saturating_add(self.sense.read_millivolts()?);
        }
        Ok(self.config.battery_millivolts(total / SAMPLES))
    }
}

impl<SENSE, CHARGING> PowerMonitor for AdcBatteryPowerMonitor<SENSE, CHARGING>
where
    SENSE: AnalogVoltageInput,
    SENSE::Error: core::fmt::Debug,
    CHARGING: InputPin,
{
    type Error = AdcBatteryError<SENSE::Error, CHARGING::Error>;

    /// Reports the battery voltage as the bus voltage; a divider measures no
    /// current.
    fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
        let millivolts = self.battery_millivolts().map_err(AdcBatteryError::Sense)?;
        Ok(PowerMeasurement {
            bus_microvolts: u64::from(millivolts) * 1000,
            shunt_nanovolts: 0,
            current_microamps: 0,
            power_microwatts: 0,
        })
    }

    fn battery(&mut self) -> Result<Option<BatteryState>, Self::Error> {
        let millivolts = self.battery_millivolts().map_err(AdcBatteryError::Sense)?;
        let charging = match self.charging.as_mut() {
            Some(input) => {
                input.is_high().map_err(AdcBatteryError::Charging)?
                    == self.config.charging_level_high
            }
            None => false,
        };
        Ok(Some(BatteryState {
            percent: self.config.percent(millivolts),
            charging,
        }))
    }
}

/// Static factory used by generated Board composition.
pub struct AdcBatteryPowerMonitorImplementation<SENSE, CHARGING>(
    PhantomData<fn() -> (SENSE, CHARGING)>,
);

impl<SENSE, CHARGING> PeripheralImplementation
    for AdcBatteryPowerMonitorImplementation<SENSE, CHARGING>
where
    SENSE: AnalogVoltageInput + 'static,
    SENSE::Error: core::fmt::Debug,
    CHARGING: InputPin + 'static,
{
    type Bindings = AdcBatteryBindings<SENSE, CHARGING>;
    type Config = AdcBatteryConfig;
    type Peripheral = AdcBatteryPowerMonitor<SENSE, CHARGING>;
    type Error = AdcBatteryInitError;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        if !config.is_valid() {
            return Err(AdcBatteryInitError::InvalidConfig);
        }
        Ok(AdcBatteryPowerMonitor {
            sense: bindings.sense,
            charging: bindings.charging,
            config,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use barracuda_board_hal::AnalogErrorType;
    use core::convert::Infallible;

    struct FixedInput(u32);

    impl AnalogErrorType for FixedInput {
        type Error = core::fmt::Error;
    }

    impl AnalogVoltageInput for FixedInput {
        fn read_millivolts(&mut self) -> Result<u32, Self::Error> {
            Ok(self.0)
        }
    }

    struct Level(bool);

    impl embedded_hal::digital::ErrorType for Level {
        type Error = Infallible;
    }

    impl InputPin for Level {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            Ok(self.0)
        }

        fn is_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.0)
        }
    }

    fn monitor(
        pin_millivolts: u32,
        config: AdcBatteryConfig,
        charging: Option<bool>,
    ) -> AdcBatteryPowerMonitor<FixedInput, Level> {
        embassy_futures::block_on(
            AdcBatteryPowerMonitorImplementation::<FixedInput, Level>::initialize(
                AdcBatteryBindings::new(FixedInput(pin_millivolts), charging.map(Level)),
                config,
            ),
        )
        .unwrap()
    }

    #[test]
    fn scales_pin_voltage_by_divider() {
        // CoreInk: 25.1 k over 5.1 k (M5Unified `_adc_ratio = 25.1f / 5.1f`).
        let config = AdcBatteryConfig::new(251, 51, 3300, 4100, false);
        let measurement = monitor(780, config, None).measure().unwrap();
        assert_eq!(measurement.bus_microvolts, 3_838_000);
        assert_eq!(measurement.current_microamps, 0);
    }

    #[test]
    fn maps_voltage_to_percent_and_clamps() {
        let config = AdcBatteryConfig::new(2, 1, 3300, 4100, false);
        assert_eq!(config.percent(3200), 0);
        assert_eq!(config.percent(3700), 50);
        assert_eq!(config.percent(4300), 100);
        let state = monitor(1850, config, None).battery().unwrap();
        assert_eq!(
            state,
            Some(BatteryState {
                percent: 50,
                charging: false
            })
        );
    }

    #[test]
    fn reads_charger_status_at_its_active_level() {
        // TinyPICO: the charger pulls GPIO34 low while it charges.
        let low_active = AdcBatteryConfig::new(602, 160, 3300, 4100, false);
        let charging = monitor(1000, low_active, Some(false)).battery().unwrap();
        assert_eq!(charging.map(|state| state.charging), Some(true));
        let idle = monitor(1000, low_active, Some(true)).battery().unwrap();
        assert_eq!(idle.map(|state| state.charging), Some(false));
    }

    #[test]
    fn rejects_degenerate_configuration() {
        let result = embassy_futures::block_on(AdcBatteryPowerMonitorImplementation::<
            FixedInput,
            Level,
        >::initialize(
            AdcBatteryBindings::new(FixedInput(0), None),
            AdcBatteryConfig::new(2, 0, 3300, 4100, false),
        ));
        assert!(matches!(result, Err(AdcBatteryInitError::InvalidConfig)));
    }
}

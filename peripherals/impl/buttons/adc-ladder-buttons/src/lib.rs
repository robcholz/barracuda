//! Buttons on a resistor ladder sampled by one ADC pin.
//!
//! Each button switches a different resistor into a divider, so the pin
//! settles at a voltage that names the button; with none pressed it rests at
//! the idle voltage. One pin therefore reports at most one pressed button.
//! A reading belongs to the button whose window holds it: the windows run
//! from halfway to the next lower level (or 0 V) to halfway to the next
//! higher level (or the idle voltage), as Espressif's board manager builds
//! them for its `adc_multi` button groups (`dev_button_sub_adc_multi.c`).

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::AnalogVoltageInput;
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonSnapshot, Buttons, MAX_BUTTONS},
};

/// The ladder's button voltages, as the Board writes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdcLadderButtonsConfig {
    levels: &'static str,
    idle_millivolts: u32,
}

impl AdcLadderButtonsConfig {
    /// Creates a configuration from space-separated ascending button
    /// voltages in millivolts and the idle voltage.
    #[must_use]
    pub const fn new(levels: &'static str, idle_millivolts: u32) -> Self {
        Self {
            levels,
            idle_millivolts,
        }
    }
}

/// Move-only ADC channel of the ladder.
pub struct AdcLadderButtonsBindings<SENSE> {
    sense: SENSE,
}

impl<SENSE> AdcLadderButtonsBindings<SENSE> {
    /// Wraps the ladder's ADC channel.
    #[must_use]
    pub const fn new(sense: SENSE) -> Self {
        Self { sense }
    }
}

/// Configuration rejected before the buttons start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdcLadderButtonsInitError {
    /// The levels are empty, more than the button API holds, not numbers,
    /// not ascending, or not below the idle voltage.
    InvalidLevels,
}

/// The window of pin voltages each button owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Windows {
    bounds: [(u32, u32); MAX_BUTTONS],
    count: usize,
}

impl Windows {
    fn parse(config: AdcLadderButtonsConfig) -> Option<Self> {
        let mut levels = [0_u32; MAX_BUTTONS];
        let mut count = 0;
        for word in config.levels.split_ascii_whitespace() {
            let level = word.parse::<u32>().ok()?;
            if count == MAX_BUTTONS || (count > 0 && level <= levels[count - 1]) {
                return None;
            }
            levels[count] = level;
            count += 1;
        }
        if count == 0 || levels[count - 1] >= config.idle_millivolts {
            return None;
        }
        let mut bounds = [(0, 0); MAX_BUTTONS];
        for index in 0..count {
            let below = if index == 0 { 0 } else { levels[index - 1] };
            let above = if index + 1 == count {
                config.idle_millivolts
            } else {
                levels[index + 1]
            };
            bounds[index] = ((below + levels[index]) / 2, (levels[index] + above) / 2);
        }
        Some(Self { bounds, count })
    }

    fn pressed(&self, millivolts: u32) -> Option<usize> {
        self.bounds[..self.count]
            .iter()
            .position(|&(low, high)| millivolts >= low && millivolts < high)
    }
}

/// Initialized ladder buttons.
pub struct AdcLadderButtons<SENSE> {
    sense: SENSE,
    windows: Windows,
}

impl<SENSE> Buttons for AdcLadderButtons<SENSE>
where
    SENSE: AnalogVoltageInput,
    SENSE::Error: core::fmt::Debug,
{
    type Error = SENSE::Error;

    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        let millivolts = self.sense.read_millivolts()?;
        let pressed = self.windows.pressed(millivolts);
        let mut states = [false; MAX_BUTTONS];
        if let Some(index) = pressed {
            states[index] = true;
        }
        Ok(ButtonSnapshot::new(&states[..self.windows.count]))
    }
}

/// Static factory used by generated Board composition.
pub struct AdcLadderButtonsImplementation<SENSE>(PhantomData<fn() -> SENSE>);

impl<SENSE> PeripheralImplementation for AdcLadderButtonsImplementation<SENSE>
where
    SENSE: AnalogVoltageInput + 'static,
    SENSE::Error: core::fmt::Debug,
{
    type Bindings = AdcLadderButtonsBindings<SENSE>;
    type Config = AdcLadderButtonsConfig;
    type Peripheral = AdcLadderButtons<SENSE>;
    type Error = AdcLadderButtonsInitError;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let windows = Windows::parse(config).ok_or(AdcLadderButtonsInitError::InvalidLevels)?;
        Ok(AdcLadderButtons {
            sense: bindings.sense,
            windows,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use barracuda_board_hal::AnalogErrorType;

    struct FixedInput(u32);

    impl AnalogErrorType for FixedInput {
        type Error = core::fmt::Error;
    }

    impl AnalogVoltageInput for FixedInput {
        fn read_millivolts(&mut self) -> Result<u32, Self::Error> {
            Ok(self.0)
        }
    }

    /// The ESP32-S3-Korvo-2 ladder (`esp32_s3_korvo_2_3` board manager).
    const KORVO2: AdcLadderButtonsConfig =
        AdcLadderButtonsConfig::new("380 820 1110 1650 1980 2410", 3000);

    fn read(config: AdcLadderButtonsConfig, millivolts: u32) -> ButtonSnapshot {
        let mut buttons =
            embassy_futures::block_on(AdcLadderButtonsImplementation::<FixedInput>::initialize(
                AdcLadderButtonsBindings::new(FixedInput(millivolts)),
                config,
            ))
            .unwrap();
        buttons.read().unwrap()
    }

    #[test]
    fn names_the_button_whose_window_holds_the_voltage() {
        let snapshot = read(KORVO2, 1100);
        assert_eq!(snapshot.len(), 6);
        assert_eq!(snapshot.is_pressed(2), Some(true));
        assert_eq!(snapshot.is_pressed(1), Some(false));
        // Window edges are halfway between neighbors: 820..1110 splits at 965.
        assert_eq!(read(KORVO2, 964).is_pressed(1), Some(true));
        assert_eq!(read(KORVO2, 965).is_pressed(2), Some(true));
        assert_eq!(read(KORVO2, 190).is_pressed(0), Some(true));
    }

    #[test]
    fn reports_no_button_at_idle() {
        let snapshot = read(KORVO2, 2900);
        assert!((0..6).all(|index| snapshot.is_pressed(index) == Some(false)));
        let snapshot = read(KORVO2, 100);
        assert!((0..6).all(|index| snapshot.is_pressed(index) == Some(false)));
    }

    #[test]
    fn rejects_invalid_levels() {
        for levels in ["", "820 380", "380 x", "380 3000", "1 2 3 4 5 6 7 8 9"] {
            let result = embassy_futures::block_on(
                AdcLadderButtonsImplementation::<FixedInput>::initialize(
                    AdcLadderButtonsBindings::new(FixedInput(0)),
                    AdcLadderButtonsConfig::new(levels, 3000),
                ),
            );
            assert!(
                matches!(result, Err(AdcLadderButtonsInitError::InvalidLevels)),
                "{levels}"
            );
        }
    }
}

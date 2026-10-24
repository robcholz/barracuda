//! Buttons on a resistor ladder sampled by one ADC pin.
//!
//! Each button switches a different resistor into a divider, so the pin
//! settles at a voltage that names the button; with none pressed it rests at
//! the idle voltage. One pin therefore reports at most one pressed button.
//! A reading belongs to the button whose window holds it: the windows run
//! from halfway to the next lower level (or 0 V) to halfway to the next
//! higher level (or the idle voltage), as Espressif's board manager builds
//! them for its `adc_multi` button groups (`dev_button_sub_adc_multi.c`).
//!
//! Readings are debounced with M5Unified's `Button_Class::setRawState` rule,
//! as for GPIO buttons, and [`Buttons::poll_event`] reports each press,
//! release, click, and long press through the shared [`ButtonTracker`]. The
//! tracker starts with every button released, so a button held while the
//! Board starts reports a press at the first sample.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::AnalogVoltageInput;
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonEvent, ButtonSnapshot, ButtonTiming, ButtonTracker, Buttons, MAX_BUTTONS},
};
use embassy_time::Instant;

/// The ladder's button voltages, as the Board writes them, and the debounce
/// and long-press times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdcLadderButtonsConfig {
    levels: &'static str,
    idle_millivolts: u32,
    timing: ButtonTiming,
}

impl AdcLadderButtonsConfig {
    /// Creates a configuration from space-separated ascending button
    /// voltages in millivolts, the idle voltage, and the debounce and
    /// long-press times.
    #[must_use]
    pub const fn new(
        levels: &'static str,
        idle_millivolts: u32,
        debounce_ms: u32,
        long_press_ms: u32,
    ) -> Self {
        Self {
            levels,
            idle_millivolts,
            timing: ButtonTiming::new(debounce_ms, long_press_ms),
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
    tracker: ButtonTracker,
}

impl<SENSE> AdcLadderButtons<SENSE>
where
    SENSE: AnalogVoltageInput,
{
    fn sample_at(&mut self, now_ms: u64) -> Result<ButtonSnapshot, SENSE::Error> {
        let millivolts = self.sense.read_millivolts()?;
        let mut states = [false; MAX_BUTTONS];
        if let Some(index) = self.windows.pressed(millivolts) {
            states[index] = true;
        }
        Ok(self.tracker.sample(&states[..self.windows.count], now_ms))
    }

    fn poll_event_at(&mut self, now_ms: u64) -> Result<Option<ButtonEvent>, SENSE::Error> {
        if !self.tracker.has_events() {
            self.sample_at(now_ms)?;
        }
        Ok(self.tracker.pop_event())
    }
}

impl<SENSE> Buttons for AdcLadderButtons<SENSE>
where
    SENSE: AnalogVoltageInput,
    SENSE::Error: core::fmt::Debug,
{
    type Error = SENSE::Error;

    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        self.sample_at(Instant::now().as_millis())
    }

    fn poll_event(&mut self) -> Result<Option<ButtonEvent>, Self::Error> {
        self.poll_event_at(Instant::now().as_millis())
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
        let released = [false; MAX_BUTTONS];
        let tracker = ButtonTracker::new(
            &released[..windows.count],
            Instant::now().as_millis(),
            config.timing,
        );
        Ok(AdcLadderButtons {
            sense: bindings.sense,
            windows,
            tracker,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    extern crate std;

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
        AdcLadderButtonsConfig::new("380 820 1110 1650 1980 2410", 3000, 10, 500);

    fn read(config: AdcLadderButtonsConfig, millivolts: u32) -> ButtonSnapshot {
        let mut buttons =
            embassy_futures::block_on(AdcLadderButtonsImplementation::<FixedInput>::initialize(
                AdcLadderButtonsBindings::new(FixedInput(millivolts)),
                config,
            ))
            .unwrap();
        // A read the debounce time after start counts the level at once.
        buttons.sample_at(Instant::now().as_millis() + 100).unwrap()
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
                    AdcLadderButtonsConfig::new(levels, 3000, 10, 500),
                ),
            );
            assert!(
                matches!(result, Err(AdcLadderButtonsInitError::InvalidLevels)),
                "{levels}"
            );
        }
    }

    /// A ladder pin whose voltage the test sets.
    struct SharedInput(std::rc::Rc<core::cell::Cell<u32>>);

    impl AnalogErrorType for SharedInput {
        type Error = core::fmt::Error;
    }

    impl AnalogVoltageInput for SharedInput {
        fn read_millivolts(&mut self) -> Result<u32, Self::Error> {
            Ok(self.0.get())
        }
    }

    #[test]
    fn moving_between_windows_releases_one_button_and_presses_the_next() {
        use barracuda_peripheral::buttons::ButtonEventKind::{Click, LongPress, Pressed, Released};

        let level = std::rc::Rc::new(core::cell::Cell::new(3000));
        let mut buttons = embassy_futures::block_on(AdcLadderButtonsImplementation::initialize(
            AdcLadderButtonsBindings::new(SharedInput(level.clone())),
            KORVO2,
        ))
        .unwrap();
        let start = Instant::now().as_millis() + 100;
        let mut events = std::vec::Vec::new();
        let mut poll = |buttons: &mut AdcLadderButtons<SharedInput>, now: u64| {
            while let Some(event) = buttons.poll_event_at(now).unwrap() {
                events.push((event.index(), event.kind()));
            }
        };
        // Button 1 down 100 ms, then straight to button 2, held 600 ms.
        level.set(820);
        poll(&mut buttons, start);
        level.set(1110);
        poll(&mut buttons, start + 100);
        for step in 1..=12 {
            poll(&mut buttons, start + 100 + 50 * step);
        }
        level.set(3000);
        poll(&mut buttons, start + 800);
        assert_eq!(
            events,
            [
                (1, Pressed),
                (1, Released),
                (1, Click),
                (2, Pressed),
                (2, LongPress),
                (2, Released),
            ]
        );
    }
}

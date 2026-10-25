//! Calibrated two-channel capacitive-touch button implementation.
//!
//! A pad counts as touched while its counter differs from its learned
//! baseline by more than the Board's threshold; an untouched pad's baseline
//! follows slow drift. Touches are debounced with M5Unified's
//! `Button_Class::setRawState` rule, as for GPIO buttons, and
//! [`Buttons::poll_event`] reports each press, release, click, and long press
//! through the shared [`ButtonTracker`].

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::CapacitiveTouchChannels;
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonEvent, ButtonSnapshot, ButtonTiming, ButtonTracker, Buttons},
};
use embassy_time::Instant;
use embedded_hal::delay::DelayNs;

/// Relative press threshold expressed in basis points of the learned
/// baseline, and the debounce and long-press times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapacitiveTouchButtonsConfig {
    threshold_basis_points: u16,
    timing: ButtonTiming,
}

impl CapacitiveTouchButtonsConfig {
    /// Creates a calibrated-button configuration.
    #[must_use]
    pub const fn new(threshold_basis_points: u16, debounce_ms: u32, long_press_ms: u32) -> Self {
        Self {
            threshold_basis_points,
            timing: ButtonTiming::new(debounce_ms, long_press_ms),
        }
    }
}

/// Move-only channels and initialization delay.
pub struct CapacitiveTouchButtonsBindings<CHANNELS, DELAY> {
    channels: CHANNELS,
    delay: DELAY,
}

impl<CHANNELS, DELAY> CapacitiveTouchButtonsBindings<CHANNELS, DELAY> {
    /// Combines the two sampled channels and delay source.
    #[must_use]
    pub const fn new(channels: CHANNELS, delay: DELAY) -> Self {
        Self { channels, delay }
    }
}

/// Initialized calibrated buttons.
pub struct CapacitiveTouchButtons<CHANNELS> {
    channels: CHANNELS,
    baseline: [u32; 2],
    threshold_basis_points: u16,
    tracker: ButtonTracker,
}

/// Static factory used by generated Board composition.
pub struct CapacitiveTouchButtonsImplementation<CHANNELS, DELAY>(
    PhantomData<fn() -> (CHANNELS, DELAY)>,
);

impl<CHANNELS, DELAY> PeripheralImplementation
    for CapacitiveTouchButtonsImplementation<CHANNELS, DELAY>
where
    CHANNELS: CapacitiveTouchChannels + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = CapacitiveTouchButtonsBindings<CHANNELS, DELAY>;
    type Config = CapacitiveTouchButtonsConfig;
    type Peripheral = CapacitiveTouchButtons<CHANNELS>;
    type Error = CHANNELS::Error;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        bindings.delay.delay_ms(100);
        let mut baseline = bindings.channels.read()?;
        for _ in 0..2 {
            bindings.delay.delay_ms(20);
            let sample = bindings.channels.read()?;
            baseline[0] = average(baseline[0], sample[0]);
            baseline[1] = average(baseline[1], sample[1]);
        }
        // Calibration assumes untouched pads.
        let tracker = ButtonTracker::new(&[false; 2], Instant::now().as_millis(), config.timing);
        Ok(CapacitiveTouchButtons {
            channels: bindings.channels,
            baseline,
            threshold_basis_points: config.threshold_basis_points,
            tracker,
        })
    }
}

impl<CHANNELS: CapacitiveTouchChannels> CapacitiveTouchButtons<CHANNELS> {
    fn sample_at(&mut self, now_ms: u64) -> Result<ButtonSnapshot, CHANNELS::Error> {
        let sample = self.channels.read()?;
        let pressed = [
            is_pressed(self.baseline[0], sample[0], self.threshold_basis_points),
            is_pressed(self.baseline[1], sample[1], self.threshold_basis_points),
        ];
        for index in 0..2 {
            if !pressed[index] {
                self.baseline[index] = tracking_average(self.baseline[index], sample[index]);
            }
        }
        Ok(self.tracker.sample(&pressed, now_ms))
    }

    fn poll_event_at(&mut self, now_ms: u64) -> Result<Option<ButtonEvent>, CHANNELS::Error> {
        if !self.tracker.has_events() {
            self.sample_at(now_ms)?;
        }
        Ok(self.tracker.pop_event())
    }
}

impl<CHANNELS: CapacitiveTouchChannels> Buttons for CapacitiveTouchButtons<CHANNELS> {
    type Error = CHANNELS::Error;

    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        self.sample_at(Instant::now().as_millis())
    }

    fn poll_event(&mut self) -> Result<Option<ButtonEvent>, Self::Error> {
        self.poll_event_at(Instant::now().as_millis())
    }
}

const fn average(left: u32, right: u32) -> u32 {
    left / 2 + right / 2 + (left % 2 + right % 2) / 2
}

const fn tracking_average(baseline: u32, sample: u32) -> u32 {
    baseline - baseline / 8 + sample / 8
}

fn is_pressed(baseline: u32, sample: u32, threshold_basis_points: u16) -> bool {
    if baseline == 0 {
        return false;
    }
    u64::from(baseline.abs_diff(sample)) * 10_000
        > u64::from(baseline) * u64::from(threshold_basis_points)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::cell::Cell;
    use core::convert::Infallible;
    use std::{rc::Rc, vec::Vec};

    use barracuda_board_hal::CapacitiveTouchChannels;
    use barracuda_peripheral::{
        PeripheralImplementation,
        buttons::ButtonEventKind::{Click, LongPress, Pressed, Released},
    };
    use embassy_time::Instant;
    use embedded_hal::delay::DelayNs;

    use super::{
        CapacitiveTouchButtons, CapacitiveTouchButtonsBindings, CapacitiveTouchButtonsConfig,
        CapacitiveTouchButtonsImplementation,
    };

    /// Two counters the test sets.
    struct Pads(Rc<Cell<[u32; 2]>>);

    impl CapacitiveTouchChannels for Pads {
        type Error = Infallible;

        fn read(&mut self) -> Result<[u32; 2], Self::Error> {
            Ok(self.0.get())
        }
    }

    struct NoDelay;

    impl DelayNs for NoDelay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    fn start(counts: &Rc<Cell<[u32; 2]>>) -> CapacitiveTouchButtons<Pads> {
        embassy_futures::block_on(CapacitiveTouchButtonsImplementation::initialize(
            CapacitiveTouchButtonsBindings::new(Pads(Rc::clone(counts)), NoDelay),
            CapacitiveTouchButtonsConfig::new(150, 10, 500),
        ))
        .unwrap()
    }

    #[test]
    fn a_count_past_the_threshold_reads_touched() {
        let counts = Rc::new(Cell::new([10_000, 20_000]));
        let mut buttons = start(&counts);
        let later = Instant::now().as_millis() + 100;
        // 1% below the baseline stays released; 2% is touched.
        counts.set([9_900, 19_600]);
        let snapshot = buttons.sample_at(later).unwrap();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.is_pressed(0), Some(false));
        assert_eq!(snapshot.is_pressed(1), Some(true));
    }

    #[test]
    fn polling_reports_a_click_and_a_long_press() {
        let counts = Rc::new(Cell::new([10_000, 10_000]));
        let mut buttons = start(&counts);
        let start = Instant::now().as_millis() + 100;
        let mut events = Vec::new();
        let mut poll = |buttons: &mut CapacitiveTouchButtons<Pads>, now: u64| {
            while let Some(event) = buttons.poll_event_at(now).unwrap() {
                events.push((event.index(), event.kind()));
            }
        };
        counts.set([10_500, 10_000]);
        poll(&mut buttons, start);
        counts.set([10_000, 10_000]);
        poll(&mut buttons, start + 100);
        counts.set([10_000, 10_500]);
        for step in 0..=12 {
            poll(&mut buttons, start + 200 + 50 * step);
        }
        counts.set([10_000, 10_000]);
        poll(&mut buttons, start + 900);
        assert_eq!(
            events,
            [
                (0, Pressed),
                (0, Released),
                (0, Click),
                (1, Pressed),
                (1, LongPress),
                (1, Released),
            ]
        );
    }
}

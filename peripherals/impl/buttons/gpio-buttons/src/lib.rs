//! Debounced buttons wired straight to SoC GPIO inputs.
//!
//! Each button is one input pin. The implementation biases every pin as the
//! Board declares, samples them all in one [`Buttons::read`], and reports a
//! button pressed when its pin sits at the Board's active level. A new level
//! counts once it has held for the debounce time; when the previous read is
//! older than that, the line has had time to settle and the new level counts
//! at once. This is M5Unified's `Button_Class::setRawState` rule, so slow
//! pollers see presses without an extra read. [`Buttons::poll_event`]
//! samples the pins when no event waits and reports each press, release,
//! click, and long press through the shared [`ButtonTracker`].

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::{ConfigurableDigitalPin, InputConfig, Pull};
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonEvent, ButtonSnapshot, ButtonTiming, ButtonTracker, Buttons, MAX_BUTTONS},
};
use embassy_time::Instant;

/// Number of optional pins after the first.
const EXTRA_BUTTONS: usize = MAX_BUTTONS.saturating_sub(1);

/// The level a pressed button puts on its pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveLevel {
    /// A press pulls the pin low, against a pull-up.
    Low,
    /// A press drives the pin high, against a pull-down.
    High,
}

/// Board-owned polarity, bias, debounce, and long-press time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpioButtonsConfig {
    active_level: ActiveLevel,
    pull: Pull,
    timing: ButtonTiming,
}

impl GpioButtonsConfig {
    /// Creates a configuration shared by every button.
    #[must_use]
    pub const fn new(
        active_level: ActiveLevel,
        pull: Pull,
        debounce_ms: u32,
        long_press_ms: u32,
    ) -> Self {
        Self {
            active_level,
            pull,
            timing: ButtonTiming::new(debounce_ms, long_press_ms),
        }
    }
}

/// Move-only button pins in index order.
pub struct GpioButtonsBindings<P> {
    first: P,
    rest: [Option<P>; EXTRA_BUTTONS],
}

impl<P> GpioButtonsBindings<P> {
    /// Combines the first button's pin with up to seven more.
    #[must_use]
    pub const fn new(first: P, rest: [Option<P>; EXTRA_BUTTONS]) -> Self {
        Self { first, rest }
    }
}

/// Button initialization or sampling failure.
#[derive(Debug)]
pub enum GpioButtonsError<E> {
    /// The Board left a gap: a button binding follows an absent one.
    NonContiguousButtons,
    /// A pin could not be configured or read.
    Pin(E),
}

/// Initialized debounced GPIO buttons.
pub struct GpioButtons<P> {
    pins: [Option<P>; MAX_BUTTONS],
    count: usize,
    active_level: ActiveLevel,
    tracker: ButtonTracker,
}

/// Static factory used by generated Board composition.
pub struct GpioButtonsImplementation<P>(PhantomData<fn() -> P>);

impl<P> PeripheralImplementation for GpioButtonsImplementation<P>
where
    P: ConfigurableDigitalPin + 'static,
{
    type Bindings = GpioButtonsBindings<P>;
    type Config = GpioButtonsConfig;
    type Peripheral = GpioButtons<P>;
    type Error = GpioButtonsError<P::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let GpioButtonsBindings { first, rest } = bindings;
        let mut pins: [Option<P>; MAX_BUTTONS] = Default::default();
        let mut count: usize = 1;
        let mut gap = false;
        pins[0] = Some(first);
        for (target, pin) in pins[1..].iter_mut().zip(rest) {
            match pin {
                Some(_) if gap => return Err(GpioButtonsError::NonContiguousButtons),
                Some(pin) => {
                    *target = Some(pin);
                    count = count.saturating_add(1);
                }
                None => gap = true,
            }
        }
        for pin in pins.iter_mut().flatten() {
            pin.configure_input(InputConfig { pull: config.pull })
                .map_err(GpioButtonsError::Pin)?;
        }
        let initial = levels(&mut pins, config.active_level)?;
        let tracker =
            ButtonTracker::new(&initial[..count], Instant::now().as_millis(), config.timing);
        Ok(GpioButtons {
            pins,
            count,
            active_level: config.active_level,
            tracker,
        })
    }
}

impl<P: ConfigurableDigitalPin> GpioButtons<P> {
    /// Returns how many buttons the Board wired.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    fn sample_at(&mut self, now_ms: u64) -> Result<ButtonSnapshot, GpioButtonsError<P::Error>> {
        let raw = levels(&mut self.pins, self.active_level)?;
        Ok(self.tracker.sample(&raw[..self.count], now_ms))
    }

    fn poll_event_at(
        &mut self,
        now_ms: u64,
    ) -> Result<Option<ButtonEvent>, GpioButtonsError<P::Error>> {
        if !self.tracker.has_events() {
            self.sample_at(now_ms)?;
        }
        Ok(self.tracker.pop_event())
    }
}

impl<P: ConfigurableDigitalPin> Buttons for GpioButtons<P> {
    type Error = GpioButtonsError<P::Error>;

    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        self.sample_at(Instant::now().as_millis())
    }

    fn poll_event(&mut self) -> Result<Option<ButtonEvent>, Self::Error> {
        self.poll_event_at(Instant::now().as_millis())
    }
}

/// Reads whether each wired pin sits at the active level.
fn levels<P: ConfigurableDigitalPin>(
    pins: &mut [Option<P>; MAX_BUTTONS],
    active_level: ActiveLevel,
) -> Result<[bool; MAX_BUTTONS], GpioButtonsError<P::Error>> {
    let mut pressed = [false; MAX_BUTTONS];
    for (pin, target) in pins.iter_mut().flatten().zip(&mut pressed) {
        let high = pin.is_high().map_err(GpioButtonsError::Pin)?;
        *target = high == (active_level == ActiveLevel::High);
    }
    Ok(pressed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::{cell::Cell, convert::Infallible};
    use std::rc::Rc;

    use barracuda_board_hal::{ConfigurableDigitalPin, InputConfig, OutputConfig, Pull};
    use barracuda_peripheral::{
        PeripheralImplementation,
        buttons::{ButtonEventKind, Buttons},
    };
    use embassy_futures::block_on;
    use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};

    use super::{
        ActiveLevel, GpioButtons, GpioButtonsBindings, GpioButtonsConfig, GpioButtonsError,
        GpioButtonsImplementation,
    };

    /// A pin whose level the test drives and whose input bias it records.
    #[derive(Clone)]
    struct TestPin {
        high: Rc<Cell<bool>>,
        pull: Rc<Cell<Option<Pull>>>,
    }

    impl TestPin {
        fn new(high: bool) -> Self {
            Self {
                high: Rc::new(Cell::new(high)),
                pull: Rc::new(Cell::new(None)),
            }
        }
    }

    impl ErrorType for TestPin {
        type Error = Infallible;
    }

    impl InputPin for TestPin {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            Ok(self.high.get())
        }

        fn is_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.high.get())
        }
    }

    impl OutputPin for TestPin {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            panic!("a button pin is never driven")
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            panic!("a button pin is never driven")
        }
    }

    impl StatefulOutputPin for TestPin {
        fn is_set_high(&mut self) -> Result<bool, Self::Error> {
            Ok(false)
        }

        fn is_set_low(&mut self) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }

    impl ConfigurableDigitalPin for TestPin {
        fn configure_input(&mut self, config: InputConfig) -> Result<(), Self::Error> {
            self.pull.set(Some(config.pull));
            Ok(())
        }

        fn configure_output(&mut self, _config: OutputConfig) -> Result<(), Self::Error> {
            panic!("a button pin is never an output")
        }

        fn disable(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn config(active_level: ActiveLevel) -> GpioButtonsConfig {
        GpioButtonsConfig::new(active_level, Pull::Up, 10, 500)
    }

    #[test]
    fn biases_every_pin_and_reports_active_low_presses_in_order() {
        let a = TestPin::new(true);
        let b = TestPin::new(false);
        let c = TestPin::new(true);
        let mut buttons = block_on(GpioButtonsImplementation::initialize(
            GpioButtonsBindings::new(
                a.clone(),
                [
                    Some(b.clone()),
                    Some(c.clone()),
                    None,
                    None,
                    None,
                    None,
                    None,
                ],
            ),
            config(ActiveLevel::Low),
        ))
        .expect("initialize buttons");

        assert_eq!(buttons.count(), 3);
        for pin in [&a, &b, &c] {
            assert_eq!(pin.pull.get(), Some(Pull::Up));
        }
        let snapshot = buttons.read().expect("read");
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot.is_pressed(0), Some(false));
        assert_eq!(snapshot.is_pressed(1), Some(true));
        assert_eq!(snapshot.is_pressed(2), Some(false));
        assert_eq!(snapshot.is_pressed(3), None);
    }

    #[test]
    fn active_high_buttons_read_a_high_pin_as_pressed() {
        let a = TestPin::new(true);
        let mut buttons = block_on(GpioButtonsImplementation::initialize(
            GpioButtonsBindings::new(a, [None, None, None, None, None, None, None]),
            config(ActiveLevel::High),
        ))
        .expect("initialize buttons");
        assert_eq!(buttons.read().expect("read").is_pressed(0), Some(true));
    }

    #[test]
    fn rejects_a_gap_between_buttons() {
        let result = block_on(GpioButtonsImplementation::initialize(
            GpioButtonsBindings::new(
                TestPin::new(true),
                [None, Some(TestPin::new(true)), None, None, None, None, None],
            ),
            config(ActiveLevel::Low),
        ));
        assert!(matches!(
            result,
            Err(GpioButtonsError::NonContiguousButtons)
        ));
    }

    #[test]
    fn a_new_level_counts_after_it_holds_for_the_debounce_time() {
        let pin = TestPin::new(true);
        let mut buttons = block_on(GpioButtonsImplementation::initialize(
            GpioButtonsBindings::new(pin.clone(), [None, None, None, None, None, None, None]),
            config(ActiveLevel::Low),
        ))
        .expect("initialize buttons");
        // A read 100 ms later finds the line settled.
        let start = embassy_time::Instant::now().as_millis() + 100;
        buttons.sample_at(start).expect("settle");

        // Polled every 2 ms: a press bouncing for 4 ms is reported 10 ms
        // after its last bounce.
        let mut reported = std::vec::Vec::new();
        for (step, high) in [false, true, false, false, false, false, false, false, false]
            .into_iter()
            .enumerate()
        {
            pin.high.set(high);
            let now = start + 2 * (step as u64 + 1);
            let snapshot = buttons.sample_at(now).expect("sample");
            reported.push(snapshot.is_pressed(0).expect("one button"));
        }
        assert_eq!(
            reported,
            [false, false, false, false, false, false, false, true, true]
        );
    }

    fn drain(
        buttons: &mut GpioButtons<TestPin>,
        now: u64,
        events: &mut std::vec::Vec<(usize, ButtonEventKind)>,
    ) {
        while let Some(event) = buttons.poll_event_at(now).expect("poll") {
            events.push((event.index(), event.kind()));
        }
    }

    #[test]
    fn polling_reports_press_long_press_release_and_click() {
        let a = TestPin::new(true);
        let b = TestPin::new(true);
        let mut buttons = block_on(GpioButtonsImplementation::initialize(
            GpioButtonsBindings::new(
                a.clone(),
                [Some(b.clone()), None, None, None, None, None, None],
            ),
            config(ActiveLevel::Low),
        ))
        .expect("initialize buttons");
        let start = embassy_time::Instant::now().as_millis() + 100;
        let mut events = std::vec::Vec::new();

        // Button B clicked: down 100 ms.
        b.high.set(false);
        drain(&mut buttons, start, &mut events);
        b.high.set(true);
        drain(&mut buttons, start + 100, &mut events);
        // Button A held 600 ms, polled every 50 ms.
        a.high.set(false);
        for step in 0..=12 {
            drain(&mut buttons, start + 200 + 50 * step, &mut events);
        }
        a.high.set(true);
        drain(&mut buttons, start + 900, &mut events);
        // The level itself is still read.
        assert_eq!(buttons.read().expect("read").is_pressed(0), Some(false));

        assert_eq!(
            events,
            [
                (1, ButtonEventKind::Pressed),
                (1, ButtonEventKind::Released),
                (1, ButtonEventKind::Click),
                (0, ButtonEventKind::Pressed),
                (0, ButtonEventKind::LongPress),
                (0, ButtonEventKind::Released),
            ]
        );
    }
}

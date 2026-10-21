//! Debounced buttons wired straight to SoC GPIO inputs.
//!
//! Each button is one input pin. The implementation biases every pin as the
//! Board declares, samples them all in one [`Buttons::read`], and reports a
//! button pressed when its pin sits at the Board's active level. A new level
//! counts once it has held for the debounce time; when the previous read is
//! older than that, the line has had time to settle and the new level counts
//! at once. This is M5Unified's `Button_Class::setRawState` rule, so slow
//! pollers see presses without an extra read.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::{ConfigurableDigitalPin, InputConfig, Pull};
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonSnapshot, Buttons, MAX_BUTTONS},
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

/// Board-owned polarity, bias, and debounce time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpioButtonsConfig {
    active_level: ActiveLevel,
    pull: Pull,
    debounce_ms: u32,
}

impl GpioButtonsConfig {
    /// Creates a configuration shared by every button.
    #[must_use]
    pub const fn new(active_level: ActiveLevel, pull: Pull, debounce_ms: u32) -> Self {
        Self {
            active_level,
            pull,
            debounce_ms,
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

/// Per-button debounce state, fed with raw samples and their time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Debounce {
    pressed: bool,
    raw: bool,
    raw_since_ms: u64,
}

impl Debounce {
    const fn settled(pressed: bool, now_ms: u64) -> Self {
        Self {
            pressed,
            raw: pressed,
            raw_since_ms: now_ms,
        }
    }

    /// Takes one raw sample and returns the debounced state.
    fn sample(&mut self, raw: bool, now_ms: u64, last_read_ms: u64, debounce_ms: u32) -> bool {
        if raw != self.raw {
            self.raw = raw;
            self.raw_since_ms = now_ms;
        }
        let debounce = u64::from(debounce_ms);
        let idle = now_ms.saturating_sub(last_read_ms) > debounce;
        if idle || now_ms.saturating_sub(self.raw_since_ms) >= debounce {
            self.pressed = raw;
        }
        self.pressed
    }
}

/// Initialized debounced GPIO buttons.
pub struct GpioButtons<P> {
    pins: [Option<P>; MAX_BUTTONS],
    state: [Debounce; MAX_BUTTONS],
    count: usize,
    config: GpioButtonsConfig,
    last_read_ms: u64,
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
        let now_ms = Instant::now().as_millis();
        let mut state = [Debounce::default(); MAX_BUTTONS];
        for (pin, debounce) in pins.iter_mut().flatten().zip(&mut state) {
            pin.configure_input(InputConfig { pull: config.pull })
                .map_err(GpioButtonsError::Pin)?;
            *debounce = Debounce::settled(is_pressed(pin, config.active_level)?, now_ms);
        }
        Ok(GpioButtons {
            pins,
            state,
            count,
            config,
            last_read_ms: now_ms,
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
        let mut pressed = [false; MAX_BUTTONS];
        for ((pin, debounce), target) in self
            .pins
            .iter_mut()
            .flatten()
            .zip(&mut self.state)
            .zip(&mut pressed)
        {
            let raw = is_pressed(pin, self.config.active_level)?;
            *target = debounce.sample(raw, now_ms, self.last_read_ms, self.config.debounce_ms);
        }
        self.last_read_ms = now_ms;
        Ok(ButtonSnapshot::new(&pressed[..self.count]))
    }
}

impl<P: ConfigurableDigitalPin> Buttons for GpioButtons<P> {
    type Error = GpioButtonsError<P::Error>;

    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        self.sample_at(Instant::now().as_millis())
    }
}

fn is_pressed<P: ConfigurableDigitalPin>(
    pin: &mut P,
    active_level: ActiveLevel,
) -> Result<bool, GpioButtonsError<P::Error>> {
    let high = pin.is_high().map_err(GpioButtonsError::Pin)?;
    Ok(high == (active_level == ActiveLevel::High))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::{cell::Cell, convert::Infallible};
    use std::rc::Rc;

    use barracuda_board_hal::{ConfigurableDigitalPin, InputConfig, OutputConfig, Pull};
    use barracuda_peripheral::{PeripheralImplementation, buttons::Buttons};
    use embassy_futures::block_on;
    use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};

    use super::{
        ActiveLevel, Debounce, GpioButtonsBindings, GpioButtonsConfig, GpioButtonsError,
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
        GpioButtonsConfig::new(active_level, Pull::Up, 10)
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
        let start = buttons.last_read_ms;

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

    #[test]
    fn a_slow_poller_sees_a_new_level_at_once() {
        let mut debounce = Debounce::settled(false, 0);
        // The previous read is 50 ms old: the press counts at once.
        assert!(debounce.sample(true, 50, 0, 10));
        // Read again within the debounce time: the release must hold.
        assert!(debounce.sample(false, 52, 50, 10));
        assert!(debounce.sample(false, 58, 52, 10));
        assert!(!debounce.sample(false, 62, 58, 10));
    }
}

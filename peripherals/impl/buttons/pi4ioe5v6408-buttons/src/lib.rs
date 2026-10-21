//! Debounced buttons on the inputs of a PI4IOE5V6408 I/O expander.
//!
//! The implementation owns only its button pins. At initialization it makes
//! them inputs with the Board's bias, through read-modify-write of the
//! direction, high-impedance, and pull registers, so pins other
//! implementations own on the same expander keep their configuration. Each
//! [`Buttons::read`] reads the input register once and never writes to the
//! expander. A new level counts once it has held for the debounce time, or at
//! once when the previous read is older than that (M5Unified
//! `Button_Class::setRawState`).

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_pi4ioe5v6408::{Error as ExpanderError, Pi4ioe5v6408};
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonSnapshot, Buttons, MAX_BUTTONS},
};
use embassy_time::Instant;
use embedded_hal::i2c::I2c;

pub use barracuda_driver_pi4ioe5v6408::Pull;

/// Number of optional buttons after the first.
const EXTRA_BUTTONS: usize = MAX_BUTTONS.saturating_sub(1);

/// The level a pressed button puts on its pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveLevel {
    /// A press pulls the pin low, against a pull-up.
    Low,
    /// A press drives the pin high, against a pull-down.
    High,
}

/// Board-owned expander address, button pins, polarity, bias, and debounce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pi4ioe5v6408ButtonsConfig {
    address: u8,
    first: u8,
    rest: [Option<u8>; EXTRA_BUTTONS],
    active_level: ActiveLevel,
    pull: Pull,
    debounce_ms: u32,
}

impl Pi4ioe5v6408ButtonsConfig {
    /// Creates a configuration: the first button's expander pin, up to seven
    /// more in index order, and settings shared by every button.
    #[must_use]
    pub const fn new(
        address: u8,
        first: u8,
        rest: [Option<u8>; EXTRA_BUTTONS],
        active_level: ActiveLevel,
        pull: Pull,
        debounce_ms: u32,
    ) -> Self {
        Self {
            address,
            first,
            rest,
            active_level,
            pull,
            debounce_ms,
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct Pi4ioe5v6408ButtonsBindings<I2C> {
    i2c: I2C,
}

impl<I2C> Pi4ioe5v6408ButtonsBindings<I2C> {
    /// Wraps the expander's I2C device view.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// Button initialization or sampling failure.
#[derive(Debug)]
pub enum Pi4ioe5v6408ButtonsError<E> {
    /// The Board left a gap between buttons or named one pin twice.
    InvalidButtonPins,
    /// An expander register operation failed.
    Expander(ExpanderError<E>),
}

impl<E> From<ExpanderError<E>> for Pi4ioe5v6408ButtonsError<E> {
    fn from(error: ExpanderError<E>) -> Self {
        Self::Expander(error)
    }
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

/// Initialized expander buttons.
pub struct Pi4ioe5v6408Buttons<I2C> {
    i2c: I2C,
    address: u8,
    pins: [u8; MAX_BUTTONS],
    count: usize,
    state: [Debounce; MAX_BUTTONS],
    active_level: ActiveLevel,
    debounce_ms: u32,
    last_read_ms: u64,
}

/// Static factory used by generated Board composition.
pub struct Pi4ioe5v6408ButtonsImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for Pi4ioe5v6408ButtonsImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Pi4ioe5v6408ButtonsBindings<I2C>;
    type Config = Pi4ioe5v6408ButtonsConfig;
    type Peripheral = Pi4ioe5v6408Buttons<I2C>;
    type Error = Pi4ioe5v6408ButtonsError<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let (pins, count) = button_pins(&config)?;
        let mut i2c = bindings.i2c;
        let mut expander = Pi4ioe5v6408::new(&mut i2c, config.address)?;
        for &pin in &pins[..count] {
            expander.configure_input(pin, config.pull)?;
        }
        let inputs = expander.read_inputs()?;
        let now_ms = Instant::now().as_millis();
        let mut state = [Debounce::default(); MAX_BUTTONS];
        for (&pin, debounce) in pins[..count].iter().zip(&mut state) {
            *debounce = Debounce::settled(is_pressed(inputs, pin, config.active_level), now_ms);
        }
        Ok(Pi4ioe5v6408Buttons {
            i2c,
            address: config.address,
            pins,
            count,
            state,
            active_level: config.active_level,
            debounce_ms: config.debounce_ms,
            last_read_ms: now_ms,
        })
    }
}

/// Collects the dense button pin list, rejecting gaps, repeats, and pins
/// outside P0 to P7.
fn button_pins<E>(
    config: &Pi4ioe5v6408ButtonsConfig,
) -> Result<([u8; MAX_BUTTONS], usize), Pi4ioe5v6408ButtonsError<E>> {
    let mut pins = [0; MAX_BUTTONS];
    let mut count: usize = 0;
    let mut seen: u8 = 0;
    let mut gap = false;
    for pin in core::iter::once(Some(config.first)).chain(config.rest) {
        let Some(pin) = pin else {
            gap = true;
            continue;
        };
        let mask = 1_u8
            .checked_shl(u32::from(pin))
            .ok_or(Pi4ioe5v6408ButtonsError::InvalidButtonPins)?;
        if gap || seen & mask != 0 {
            return Err(Pi4ioe5v6408ButtonsError::InvalidButtonPins);
        }
        seen |= mask;
        let slot = pins
            .get_mut(count)
            .ok_or(Pi4ioe5v6408ButtonsError::InvalidButtonPins)?;
        *slot = pin;
        count = count.saturating_add(1);
    }
    Ok((pins, count))
}

impl<I2C: I2c> Pi4ioe5v6408Buttons<I2C> {
    /// Returns how many buttons the Board wired.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    fn sample_at(
        &mut self,
        now_ms: u64,
    ) -> Result<ButtonSnapshot, Pi4ioe5v6408ButtonsError<I2C::Error>> {
        let inputs = Pi4ioe5v6408::new(&mut self.i2c, self.address)?.read_inputs()?;
        let mut pressed = [false; MAX_BUTTONS];
        for ((&pin, debounce), target) in self.pins[..self.count]
            .iter()
            .zip(&mut self.state)
            .zip(&mut pressed)
        {
            let raw = is_pressed(inputs, pin, self.active_level);
            *target = debounce.sample(raw, now_ms, self.last_read_ms, self.debounce_ms);
        }
        self.last_read_ms = now_ms;
        Ok(ButtonSnapshot::new(&pressed[..self.count]))
    }
}

impl<I2C: I2c> Buttons for Pi4ioe5v6408Buttons<I2C> {
    type Error = Pi4ioe5v6408ButtonsError<I2C::Error>;

    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        self.sample_at(Instant::now().as_millis())
    }
}

fn is_pressed(inputs: u8, pin: u8, active_level: ActiveLevel) -> bool {
    let high = inputs.checked_shr(u32::from(pin)).unwrap_or_default() & 1 != 0;
    high == (active_level == ActiveLevel::High)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use barracuda_peripheral::{PeripheralImplementation, buttons::Buttons};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{
        ActiveLevel, Pi4ioe5v6408ButtonsBindings, Pi4ioe5v6408ButtonsConfig,
        Pi4ioe5v6408ButtonsError, Pi4ioe5v6408ButtonsImplementation, Pull,
    };

    const DIRECTION: usize = 0x03;
    const HIGH_IMPEDANCE: usize = 0x07;
    const PULL_ENABLE: usize = 0x0b;
    const PULL_SELECTION: usize = 0x0d;
    const INPUT: usize = 0x0f;

    /// A register-file model of the expander that logs every write.
    struct Expander {
        registers: [u8; 0x12],
        writes: Vec<(u8, u8)>,
    }

    impl Expander {
        fn new() -> Self {
            let mut registers = [0; 0x12];
            // The display's backlight output on P7, and every input high.
            registers[DIRECTION] = 0x80;
            registers[HIGH_IMPEDANCE] = 0x7f;
            registers[INPUT] = 0xff;
            Self {
                registers,
                writes: Vec::new(),
            }
        }
    }

    impl ErrorType for Expander {
        type Error = Infallible;
    }

    impl I2c for Expander {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x43);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = usize::from(*reg),
                    Operation::Write([reg, value]) => {
                        self.registers[usize::from(*reg)] = *value;
                        self.writes.push((*reg, *value));
                    }
                    Operation::Read(bytes) => bytes.fill(self.registers[register]),
                    Operation::Write(_) => {}
                }
            }
            Ok(())
        }
    }

    fn stamp_plc() -> Pi4ioe5v6408ButtonsConfig {
        // M5Unified StampPLC: buttons A, B, C on P2, P1, P0 with pull-ups.
        Pi4ioe5v6408ButtonsConfig::new(
            0x43,
            2,
            [Some(1), Some(0), None, None, None, None, None],
            ActiveLevel::Low,
            Pull::Up,
            10,
        )
    }

    #[test]
    fn configures_only_the_button_pins_and_keeps_the_backlight_output() {
        let mut buttons = block_on(Pi4ioe5v6408ButtonsImplementation::initialize(
            Pi4ioe5v6408ButtonsBindings::new(Expander::new()),
            stamp_plc(),
        ))
        .expect("initialize buttons");
        assert_eq!(buttons.count(), 3);
        let registers = buttons.i2c.registers;
        assert_eq!(registers[DIRECTION], 0x80);
        assert_eq!(registers[HIGH_IMPEDANCE], 0x7f);
        assert_eq!(registers[PULL_ENABLE], 0x07);
        assert_eq!(registers[PULL_SELECTION], 0x07);

        buttons.i2c.writes.clear();
        // Button B (P1) held low.
        buttons.i2c.registers[INPUT] = 0b1111_1101;
        let snapshot = buttons.sample_at(buttons.last_read_ms + 100).expect("read");
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot.is_pressed(0), Some(false));
        assert_eq!(snapshot.is_pressed(1), Some(true));
        assert_eq!(snapshot.is_pressed(2), Some(false));
        assert!(buttons.i2c.writes.is_empty(), "reads never write");
        assert_eq!(buttons.read().expect("read").len(), 3);
    }

    #[test]
    fn rejects_gaps_repeats_and_pins_past_p7() {
        for rest in [
            [None, Some(0), None, None, None, None, None],
            [Some(2), None, None, None, None, None, None],
            [Some(8), None, None, None, None, None, None],
        ] {
            let config =
                Pi4ioe5v6408ButtonsConfig::new(0x43, 2, rest, ActiveLevel::Low, Pull::Up, 10);
            let result = block_on(Pi4ioe5v6408ButtonsImplementation::initialize(
                Pi4ioe5v6408ButtonsBindings::new(Expander::new()),
                config,
            ));
            assert!(matches!(
                result,
                Err(Pi4ioe5v6408ButtonsError::InvalidButtonPins)
            ));
        }
    }
}

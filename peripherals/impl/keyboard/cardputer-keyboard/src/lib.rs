//! M5Stack Cardputer keyboard: a 56-key matrix the SoC scans itself.
//!
//! Three outputs drive the address inputs of a 74HC138, whose eight outputs
//! pull one scan line low at a time; seven inputs with pull-ups read the keys
//! pressed on that line as low. The scan and the mapping of a scan line and
//! input to the 4 by 14 layout are M5Cardputer's `IOMatrixKeyboardReader`;
//! the keymap is [`layout`].
//!
//! Each [`Keyboard::poll_event`] that finds no change waiting scans the
//! whole matrix once, then reports changed keys one at a time, modifiers
//! first, so a letter pressed together with Shift types its second legend.

#![no_std]

pub mod layout;

use core::marker::PhantomData;

use barracuda_board_hal::{ConfigurableDigitalPin, InputConfig, Pull};
use barracuda_peripheral::{
    PeripheralImplementation,
    keyboard::{KeyEvent, Keyboard},
};
use embedded_hal::{
    delay::DelayNs,
    digital::{OutputPin, PinState},
};

use layout::{COLUMNS, KEYS, Layout, is_modifier};

/// Scan lines the 74HC138 decodes.
const SCAN_LINES: u8 = 8;
/// Inputs read on each scan line.
const INPUTS: usize = 7;
/// Time for an input to rise through its pull-up after the decoder releases
/// its line, before it is read. M5Cardputer reads right after Arduino's
/// `digitalWrite`, whose overhead is about this long.
const SETTLE_US: u32 = 5;

/// Move-only address outputs, scan inputs, and settling delay.
pub struct CardputerKeyboardBindings<O, I, D> {
    address: [O; 3],
    inputs: [I; INPUTS],
    delay: D,
}

impl<O, I, D> CardputerKeyboardBindings<O, I, D> {
    /// Combines the 74HC138's A0, A1, A2 outputs, the seven inputs in
    /// M5Cardputer's `input_list` order, and a delay.
    #[must_use]
    pub const fn new(address: [O; 3], inputs: [I; INPUTS], delay: D) -> Self {
        Self {
            address,
            inputs,
            delay,
        }
    }
}

/// Keyboard set-up or scanning failure.
#[derive(Debug)]
pub enum CardputerKeyboardError<O, I> {
    /// An address output failed.
    Output(O),
    /// An input could not be configured or read.
    Input(I),
}

/// Initialized Cardputer keyboard.
pub struct CardputerKeyboard<O, I, D> {
    address: [O; 3],
    inputs: [I; INPUTS],
    delay: D,
    layout: Layout,
    modifiers: u64,
    scanned: u64,
    reported: u64,
}

/// Static factory used by generated Board composition.
pub struct CardputerKeyboardImplementation<O, I, D>(PhantomData<Bindings<O, I, D>>);

/// The binding types an implementation is generic over.
type Bindings<O, I, D> = fn() -> (O, I, D);

impl<O, I, D> PeripheralImplementation for CardputerKeyboardImplementation<O, I, D>
where
    O: OutputPin + 'static,
    I: ConfigurableDigitalPin + 'static,
    D: DelayNs + 'static,
{
    type Bindings = CardputerKeyboardBindings<O, I, D>;
    type Config = ();
    type Peripheral = CardputerKeyboard<O, I, D>;
    type Error = CardputerKeyboardError<O::Error, I::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        _config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let CardputerKeyboardBindings {
            address,
            mut inputs,
            delay,
        } = bindings;
        for input in &mut inputs {
            input
                .configure_input(InputConfig { pull: Pull::Up })
                .map_err(CardputerKeyboardError::Input)?;
        }
        let mut keyboard = CardputerKeyboard {
            address,
            inputs,
            delay,
            layout: Layout::new(),
            modifiers: modifier_mask(),
            scanned: 0,
            reported: 0,
        };
        // Keys held at start are not reported as pressed.
        keyboard.scanned = keyboard.scan()?;
        keyboard.reported = keyboard.scanned;
        Ok(keyboard)
    }
}

impl<O, I, D> CardputerKeyboard<O, I, D>
where
    O: OutputPin,
    I: ConfigurableDigitalPin,
    D: DelayNs,
{
    /// Samples every key; bit `row * 14 + column` is set while it is down.
    fn scan(&mut self) -> Result<u64, CardputerKeyboardError<O::Error, I::Error>> {
        let mut pressed = 0_u64;
        for line in 0..SCAN_LINES {
            for (bit, output) in self.address.iter_mut().enumerate() {
                output
                    .set_state(PinState::from(
                        line.checked_shr(bit_index(bit)).unwrap_or(0) & 1 != 0,
                    ))
                    .map_err(CardputerKeyboardError::Output)?;
            }
            self.delay.delay_us(SETTLE_US);
            for (input, pin) in self.inputs.iter_mut().enumerate() {
                if pin.is_low().map_err(CardputerKeyboardError::Input)? {
                    pressed |= bit(key_index(line, input));
                }
            }
        }
        Ok(pressed)
    }
}

/// M5Cardputer `IOMatrixKeyboardReader::update`: lines 4 to 7 hold the even
/// columns and lines 0 to 3 the odd ones, and the layout's row is
/// `3 - line % 4`.
const fn key_index(line: u8, input: usize) -> usize {
    let column = input
        .saturating_mul(2)
        .saturating_add(if line > 3 { 0 } else { 1 });
    let row = 3_usize.saturating_sub((line & 3) as usize);
    row.saturating_mul(COLUMNS).saturating_add(column)
}

fn bit_index(bit: usize) -> u32 {
    u32::try_from(bit).unwrap_or(u32::MAX)
}

/// The bit of key `index` in a scan.
fn bit(index: usize) -> u64 {
    u32::try_from(index)
        .ok()
        .and_then(|shift| 1_u64.checked_shl(shift))
        .unwrap_or(0)
}

/// The layout row and column of key `index`.
fn position(index: usize) -> (usize, usize) {
    (
        index.checked_div(COLUMNS).unwrap_or(0),
        index.checked_rem(COLUMNS).unwrap_or(0),
    )
}

/// Bits of the modifier keys, reported before the others.
fn modifier_mask() -> u64 {
    (0..KEYS)
        .filter(|&index| {
            let (row, column) = position(index);
            is_modifier(row, column)
        })
        .fold(0, |mask, index| mask | bit(index))
}

impl<O, I, D> Keyboard for CardputerKeyboard<O, I, D>
where
    O: OutputPin,
    I: ConfigurableDigitalPin,
    D: DelayNs,
{
    type Error = CardputerKeyboardError<O::Error, I::Error>;

    fn poll_event(&mut self) -> Result<Option<KeyEvent>, Self::Error> {
        if self.reported == self.scanned {
            self.scanned = self.scan()?;
        }
        let changed = self.reported ^ self.scanned;
        if changed == 0 {
            return Ok(None);
        }
        let modifiers = changed & self.modifiers;
        let next = if modifiers != 0 { modifiers } else { changed };
        let index = usize::try_from(next.trailing_zeros()).unwrap_or(0);
        let key = bit(index);
        self.reported ^= key;
        let (row, column) = position(index);
        Ok(self.layout.translate(row, column, self.scanned & key != 0))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::{cell::Cell, convert::Infallible};
    use std::rc::Rc;

    use barracuda_board_hal::{ConfigurableDigitalPin, InputConfig, OutputConfig, Pull};
    use barracuda_peripheral::{
        PeripheralImplementation,
        keyboard::{Key, Keyboard},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin},
    };

    use super::{CardputerKeyboardBindings, CardputerKeyboardImplementation, key_index};

    /// The decoder's address, and which (line, input) pairs are pressed.
    #[derive(Default)]
    struct Matrix {
        line: Cell<u8>,
        pressed: Cell<u64>,
        pull_ups: Cell<u8>,
    }

    struct Address(Rc<Matrix>, u8);

    impl ErrorType for Address {
        type Error = Infallible;
    }

    impl OutputPin for Address {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.0.line.set(self.0.line.get() & !(1 << self.1));
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.0.line.set(self.0.line.get() | (1 << self.1));
            Ok(())
        }
    }

    struct Input(Rc<Matrix>, usize);

    impl ErrorType for Input {
        type Error = Infallible;
    }

    impl InputPin for Input {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.is_low()?)
        }

        fn is_low(&mut self) -> Result<bool, Self::Error> {
            let line = usize::from(self.0.line.get());
            Ok(self.0.pressed.get() & (1 << (line * 7 + self.1)) != 0)
        }
    }

    impl OutputPin for Input {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl StatefulOutputPin for Input {
        fn is_set_high(&mut self) -> Result<bool, Self::Error> {
            Ok(false)
        }

        fn is_set_low(&mut self) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }

    impl ConfigurableDigitalPin for Input {
        fn configure_input(&mut self, config: InputConfig) -> Result<(), Self::Error> {
            if config.pull == Pull::Up {
                self.0.pull_ups.set(self.0.pull_ups.get() + 1);
            }
            Ok(())
        }

        fn configure_output(&mut self, _config: OutputConfig) -> Result<(), Self::Error> {
            Ok(())
        }

        fn disable(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct Delay;

    impl DelayNs for Delay {
        fn delay_ns(&mut self, _nanoseconds: u32) {}
    }

    fn press(matrix: &Matrix, line: u8, input: usize) {
        matrix
            .pressed
            .set(matrix.pressed.get() | (1 << (usize::from(line) * 7 + input)));
    }

    fn release(matrix: &Matrix, line: u8, input: usize) {
        matrix
            .pressed
            .set(matrix.pressed.get() & !(1 << (usize::from(line) * 7 + input)));
    }

    #[test]
    fn maps_scan_lines_like_m5cardputer() {
        // Line 7, input 0 is column 0 of the top row.
        assert_eq!(key_index(7, 0), 0);
        // Line 3, input 0 is column 1 of the top row.
        assert_eq!(key_index(3, 0), 1);
        // Line 0, input 6 is column 13 of the bottom row: the space bar.
        assert_eq!(key_index(0, 6), 55);
    }

    #[test]
    fn reports_presses_and_releases_with_modifiers_first() {
        let matrix = Rc::new(Matrix::default());
        let mut keyboard = block_on(CardputerKeyboardImplementation::initialize(
            CardputerKeyboardBindings::new(
                [0, 1, 2].map(|bit| Address(Rc::clone(&matrix), bit)),
                [0, 1, 2, 3, 4, 5, 6].map(|input| Input(Rc::clone(&matrix), input)),
                Delay,
            ),
            (),
        ))
        .expect("initialize");
        assert_eq!(matrix.pull_ups.get(), 7);
        assert_eq!(keyboard.poll_event().expect("poll"), None);

        // Row 2 is line 1 (odd columns) and line 5 (even columns): `a` is
        // column 2 (line 5, input 1), Shift (Aa) column 1 (line 1, input 0).
        press(&matrix, 5, 1);
        press(&matrix, 1, 0);
        let shift = keyboard.poll_event().expect("poll").expect("shift");
        assert_eq!(shift.key(), Key::Shift);
        let a = keyboard.poll_event().expect("poll").expect("a");
        assert_eq!((a.key(), a.text()), (Key::Character('a'), Some('A')));
        assert_eq!(keyboard.poll_event().expect("poll"), None);

        release(&matrix, 5, 1);
        let up = keyboard.poll_event().expect("poll").expect("release");
        assert!(!up.is_pressed());
        assert_eq!(up.key(), Key::Character('a'));
    }
}

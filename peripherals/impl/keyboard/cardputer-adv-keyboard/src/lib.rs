//! M5Stack Cardputer-ADV keyboard: the Cardputer's layout behind a TCA8418.
//!
//! The TCA8418 scans and debounces a 7-row, 8-column matrix and queues press
//! and release events. M5Cardputer's `TCA8418KeyboardReader` maps a
//! controller row and column to the Cardputer's 4 by 14 layout: column
//! `row * 2 + (column > 3)`, row `column % 4`. With the interrupt line wired,
//! a poll reads nothing over I2C while the line is high.

#![no_std]

use core::marker::PhantomData;

use barracuda_cardputer_keyboard::layout::Layout;
use barracuda_driver_tca8418::{Error as ControllerError, FIFO_DEPTH, Tca8418};
use barracuda_peripheral::{
    PeripheralImplementation,
    keyboard::{KeyEvent, Keyboard},
};
use embedded_hal::{digital::InputPin, i2c::I2c};

/// Matrix rows the Cardputer-ADV wires (M5Cardputer `matrix(7, 8)`).
const ROWS: u8 = 7;
/// Matrix columns the Cardputer-ADV wires.
const COLUMNS: u8 = 8;

/// Board-owned controller address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardputerAdvKeyboardConfig {
    address: u8,
}

impl CardputerAdvKeyboardConfig {
    /// Creates configuration for the controller at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Move-only I2C device view and optional interrupt line.
pub struct CardputerAdvKeyboardBindings<I2C, INT> {
    i2c: I2C,
    interrupt: Option<INT>,
}

impl<I2C, INT> CardputerAdvKeyboardBindings<I2C, INT> {
    /// Combines the controller's bus and its interrupt line, when wired.
    #[must_use]
    pub const fn new(i2c: I2C, interrupt: Option<INT>) -> Self {
        Self { i2c, interrupt }
    }
}

/// Keyboard set-up or reading failure.
#[derive(Debug)]
pub enum CardputerAdvKeyboardError<BusError, PinError> {
    /// The TCA8418 rejected its configuration or the bus failed.
    Controller(ControllerError<BusError>),
    /// The interrupt line could not be read.
    Interrupt(PinError),
}

/// Initialized Cardputer-ADV keyboard.
pub struct CardputerAdvKeyboard<I2C, INT> {
    controller: Tca8418<I2C>,
    interrupt: Option<INT>,
    layout: Layout,
}

/// Static factory used by generated Board composition.
pub struct CardputerAdvKeyboardImplementation<I2C, INT>(PhantomData<fn() -> (I2C, INT)>);

impl<I2C, INT> PeripheralImplementation for CardputerAdvKeyboardImplementation<I2C, INT>
where
    I2C: I2c + 'static,
    INT: InputPin + 'static,
{
    type Bindings = CardputerAdvKeyboardBindings<I2C, INT>;
    type Config = CardputerAdvKeyboardConfig;
    type Peripheral = CardputerAdvKeyboard<I2C, INT>;
    type Error = CardputerAdvKeyboardError<I2C::Error, INT::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let mut controller = Tca8418::new(bindings.i2c, config.address)
            .map_err(CardputerAdvKeyboardError::Controller)?;
        controller
            .initialize(ROWS, COLUMNS)
            .map_err(CardputerAdvKeyboardError::Controller)?;
        Ok(CardputerAdvKeyboard {
            controller,
            interrupt: bindings.interrupt,
            layout: Layout::new(),
        })
    }
}

impl<I2C, INT> Keyboard for CardputerAdvKeyboard<I2C, INT>
where
    I2C: I2c,
    INT: InputPin,
{
    type Error = CardputerAdvKeyboardError<I2C::Error, INT::Error>;

    fn poll_event(&mut self) -> Result<Option<KeyEvent>, Self::Error> {
        if let Some(interrupt) = &mut self.interrupt
            && interrupt
                .is_high()
                .map_err(CardputerAdvKeyboardError::Interrupt)?
        {
            return Ok(None);
        }
        for _ in 0..FIFO_DEPTH {
            let event = self
                .controller
                .read_event()
                .map_err(CardputerAdvKeyboardError::Controller)?;
            // The controller keeps its interrupt raised while events wait.
            self.controller
                .clear_key_interrupt()
                .map_err(CardputerAdvKeyboardError::Controller)?;
            let Some(event) = event else {
                return Ok(None);
            };
            let Some((row, column)) = event.matrix_position() else {
                continue;
            };
            let (layout_row, layout_column) = remap(row, column);
            if let Some(key) = self
                .layout
                .translate(layout_row, layout_column, event.is_pressed())
            {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }
}

/// M5Cardputer `TCA8418KeyboardReader::remap`.
const fn remap(row: u8, column: u8) -> (usize, usize) {
    let layout_column = (row as usize)
        .saturating_mul(2)
        .saturating_add(if column > 3 { 1 } else { 0 });
    ((column & 3) as usize, layout_column)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use core::{cell::RefCell, convert::Infallible};
    use std::{collections::VecDeque, rc::Rc};

    use barracuda_peripheral::{
        PeripheralImplementation,
        keyboard::{Key, Keyboard},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        digital::{ErrorType as PinErrorType, InputPin},
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{
        CardputerAdvKeyboardBindings, CardputerAdvKeyboardConfig,
        CardputerAdvKeyboardImplementation, remap,
    };

    #[derive(Default)]
    struct State {
        fifo: VecDeque<u8>,
        configured_rows: u8,
    }

    #[derive(Default)]
    struct Controller(Rc<RefCell<State>>);

    impl ErrorType for Controller {
        type Error = Infallible;
    }

    impl I2c for Controller {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([0x1D, value]) => self.0.borrow_mut().configured_rows = *value,
                    Operation::Write([target]) => register = *target,
                    Operation::Write(_) => {}
                    Operation::Read([value]) => {
                        *value = if register == 0x04 {
                            self.0.borrow_mut().fifo.pop_front().unwrap_or(0)
                        } else {
                            0
                        };
                    }
                    Operation::Read(_) => {}
                }
            }
            Ok(())
        }
    }

    struct Line(bool);

    impl PinErrorType for Line {
        type Error = Infallible;
    }

    impl InputPin for Line {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            Ok(self.0)
        }

        fn is_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.0)
        }
    }

    #[test]
    fn remaps_like_m5cardputer() {
        // Controller row 1, column 6: layout row 2, column 3 (`s`).
        assert_eq!(remap(1, 6), (2, 3));
        assert_eq!(remap(0, 0), (0, 0));
        assert_eq!(remap(6, 7), (3, 13));
    }

    #[test]
    fn reads_the_event_fifo() {
        let state = Rc::new(RefCell::new(State::default()));
        // Initialization flushes what was queued before.
        state.borrow_mut().fifo.extend([0x80 | 17, 17]);
        let mut keyboard = block_on(CardputerAdvKeyboardImplementation::initialize(
            CardputerAdvKeyboardBindings::new(Controller(Rc::clone(&state)), Some(Line(false))),
            CardputerAdvKeyboardConfig::new(0x34),
        ))
        .expect("initialize");
        assert_eq!(state.borrow().configured_rows, 0x7F);
        assert!(state.borrow().fifo.is_empty());
        assert_eq!(keyboard.poll_event().expect("poll"), None);

        // Key 17 is controller row 1, column 6: `s` pressed then released.
        state.borrow_mut().fifo.extend([0x80 | 17, 17]);
        let press = keyboard.poll_event().expect("poll").expect("press");
        assert_eq!(
            (press.key(), press.text()),
            (Key::Character('s'), Some('s'))
        );
        let release = keyboard.poll_event().expect("poll").expect("release");
        assert!(!release.is_pressed());
        assert_eq!(keyboard.poll_event().expect("poll"), None);
        // An idle interrupt line skips the bus.
        state.borrow_mut().fifo.push_back(0x80 | 17);
        keyboard.interrupt = Some(Line(true));
        assert_eq!(keyboard.poll_event().expect("poll"), None);
        assert_eq!(state.borrow().fifo.len(), 1);
    }
}

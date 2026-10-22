//! Low-level Driver for the Texas Instruments TCA8418 keypad-scan controller.
//!
//! The TCA8418 scans a key matrix of up to eight rows and ten columns on its
//! own, debounces it, and queues up to ten press and release events in a
//! FIFO, raising its open-drain interrupt line while events wait. The
//! register map and the set-up sequence follow TI's datasheet (SCPS215) and
//! Adafruit's TCA8418 library, as M5Stack ships it for the Cardputer-ADV.

#![no_std]

use embedded_hal::i2c::I2c;

/// The TCA8418's fixed I2C address.
pub const ADDRESS: u8 = 0x34;

/// Most rows the controller scans.
pub const MAX_ROWS: u8 = 8;
/// Most columns the controller scans.
pub const MAX_COLUMNS: u8 = 10;
/// Depth of the key-event FIFO.
pub const FIFO_DEPTH: usize = 10;

const CFG: u8 = 0x01;
const INT_STAT: u8 = 0x02;
const KEY_LCK_EC: u8 = 0x03;
const KEY_EVENT_A: u8 = 0x04;
const GPIO_INT_STAT_1: u8 = 0x11;
const GPIO_INT_EN_1: u8 = 0x1A;
const KP_GPIO_1: u8 = 0x1D;
const KP_GPIO_2: u8 = 0x1E;
const KP_GPIO_3: u8 = 0x1F;
const GPI_EM_1: u8 = 0x20;
const GPIO_DIR_1: u8 = 0x23;
const GPIO_INT_LVL_1: u8 = 0x26;

/// `CFG.KE_IEN`: key events raise the interrupt line.
const CFG_KEY_EVENT_INTERRUPT: u8 = 0x01;
/// `CFG.GPI_IEN`: GPI events raise the interrupt line.
const CFG_GPI_INTERRUPT: u8 = 0x02;
/// `INT_STAT.K_INT`: a key event is waiting; writing one clears it.
const INT_STAT_KEY: u8 = 0x01;
/// `INT_STAT.GPI_INT`.
const INT_STAT_GPI: u8 = 0x02;
/// `KEY_LCK_EC.EC`: the number of events in the FIFO.
const EVENT_COUNT: u8 = 0x0F;
/// Key event bit 7: set for a press, clear for a release.
const EVENT_PRESSED: u8 = 0x80;
const EVENT_CODE: u8 = 0x7F;

/// TCA8418 configuration or register failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The matrix has no rows or columns, or more than the controller scans.
    InvalidMatrix,
    /// An I2C register operation failed.
    Bus(BusError),
}

/// One press or release taken from the event FIFO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    code: u8,
    pressed: bool,
}

impl KeyEvent {
    /// Decodes one `KEY_EVENT_A` value; `None` for zero, an empty FIFO.
    #[must_use]
    pub const fn from_register(value: u8) -> Option<Self> {
        if value == 0 {
            return None;
        }
        Some(Self {
            code: value & EVENT_CODE,
            pressed: value & EVENT_PRESSED != 0,
        })
    }

    /// Returns the key number: `row * 10 + column + 1` for a matrix key,
    /// above 96 for a key on a GPI pin.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.code
    }

    /// Returns whether the key went down.
    #[must_use]
    pub const fn is_pressed(self) -> bool {
        self.pressed
    }

    /// Returns the matrix row and column of a matrix key; `None` for a GPI
    /// event.
    #[must_use]
    pub const fn matrix_position(self) -> Option<(u8, u8)> {
        match self.code.checked_sub(1) {
            Some(index) if index < MAX_ROWS * MAX_COLUMNS => {
                Some((index / MAX_COLUMNS, index % MAX_COLUMNS))
            }
            _ => None,
        }
    }
}

/// One TCA8418 on an I2C bus.
pub struct Tca8418<I2C> {
    i2c: I2C,
    address: u8,
}

impl<I2C> Tca8418<I2C>
where
    I2C: I2c,
{
    /// Takes the bus for the controller at `address`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidAddress`] for a non-seven-bit address.
    pub fn new(i2c: I2C, address: u8) -> Result<Self, Error<I2C::Error>> {
        if address > 0x7F {
            return Err(Error::InvalidAddress);
        }
        Ok(Self { i2c, address })
    }

    /// Returns the bus.
    pub fn release(self) -> I2C {
        self.i2c
    }

    /// Scans rows `0..rows` and columns `0..columns` as a key matrix,
    /// discards events queued before, and raises the interrupt line for
    /// key events: Adafruit's `begin`, `matrix`, `flush`, and
    /// `enableInterrupts`, in M5Stack's order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidMatrix`] for an empty or oversized matrix, or
    /// an I2C register error.
    pub fn initialize(&mut self, rows: u8, columns: u8) -> Result<(), Error<I2C::Error>> {
        if rows == 0 || columns == 0 || rows > MAX_ROWS || columns > MAX_COLUMNS {
            return Err(Error::InvalidMatrix);
        }
        // Every pin an input, reporting falling-edge GPI events, until the
        // keypad selection below claims the matrix pins.
        for (base, value) in [
            (GPIO_DIR_1, 0x00),
            (GPI_EM_1, 0xFF),
            (GPIO_INT_LVL_1, 0x00),
            (GPIO_INT_EN_1, 0xFF),
        ] {
            for offset in 0..3 {
                self.write(base.wrapping_add(offset), value)?;
            }
        }
        self.write(KP_GPIO_1, low_bits(rows))?;
        self.write(KP_GPIO_2, low_bits(columns.min(8)))?;
        if columns > 8 {
            self.write(KP_GPIO_3, low_bits(columns.wrapping_sub(8)))?;
        }
        self.flush()?;
        let config = self.read(CFG)?;
        self.write(CFG, config | CFG_GPI_INTERRUPT | CFG_KEY_EVENT_INTERRUPT)
    }

    /// Discards every queued event and clears the interrupt status.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn flush(&mut self) -> Result<(), Error<I2C::Error>> {
        for _ in 0..FIFO_DEPTH {
            if self.read_event()?.is_none() {
                break;
            }
        }
        for offset in 0..3 {
            self.read(GPIO_INT_STAT_1.wrapping_add(offset))?;
        }
        self.write(INT_STAT, INT_STAT_KEY | INT_STAT_GPI)
    }

    /// Returns how many events wait in the FIFO.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn event_count(&mut self) -> Result<u8, Error<I2C::Error>> {
        Ok(self.read(KEY_LCK_EC)? & EVENT_COUNT)
    }

    /// Takes the oldest event from the FIFO; `None` when it is empty.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn read_event(&mut self) -> Result<Option<KeyEvent>, Error<I2C::Error>> {
        Ok(KeyEvent::from_register(self.read(KEY_EVENT_A)?))
    }

    /// Clears the key-event interrupt. The controller keeps it, and its
    /// interrupt line, while events still wait.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn clear_key_interrupt(&mut self) -> Result<(), Error<I2C::Error>> {
        self.write(INT_STAT, INT_STAT_KEY)
    }

    fn read(&mut self, register: u8) -> Result<u8, Error<I2C::Error>> {
        let mut value = [0];
        self.i2c
            .write_read(self.address, &[register], &mut value)
            .map_err(Error::Bus)?;
        Ok(value[0])
    }

    fn write(&mut self, register: u8, value: u8) -> Result<(), Error<I2C::Error>> {
        self.i2c
            .write(self.address, &[register, value])
            .map_err(Error::Bus)
    }
}

/// A mask of the lowest `count` bits, `count` at most eight.
const fn low_bits(count: u8) -> u8 {
    match 1_u16.checked_shl(count as u32) {
        Some(bit) => bit.wrapping_sub(1) as u8,
        None => 0xFF,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::collections::VecDeque;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{ADDRESS, Error, KeyEvent, Tca8418};

    /// A TCA8418 register file with an event FIFO.
    struct Chip {
        registers: [u8; 0x30],
        fifo: VecDeque<u8>,
        writes: std::vec::Vec<(u8, u8)>,
    }

    impl Default for Chip {
        fn default() -> Self {
            Self {
                registers: [0; 0x30],
                fifo: VecDeque::new(),
                writes: std::vec::Vec::new(),
            }
        }
    }

    impl ErrorType for Chip {
        type Error = Infallible;
    }

    impl I2c for Chip {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, ADDRESS);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([target, value]) => {
                        self.writes.push((*target, *value));
                        if *target == 0x02 {
                            // INT_STAT bits clear when written with one.
                            self.registers[2] &= !*value;
                        } else {
                            self.registers[usize::from(*target)] = *value;
                        }
                    }
                    Operation::Write([target]) => register = *target,
                    Operation::Write(_) => {}
                    Operation::Read([value]) => {
                        *value = match register {
                            0x03 => self.fifo.len() as u8,
                            0x04 => self.fifo.pop_front().unwrap_or(0),
                            other => self.registers[usize::from(other)],
                        };
                    }
                    Operation::Read(_) => {}
                }
            }
            Ok(())
        }
    }

    #[test]
    fn sets_up_m5stacks_seven_by_eight_matrix() {
        let mut chip = Chip::default();
        chip.fifo.extend([0x81, 0x01]);
        chip.registers[2] = 0x03;
        let mut controller = Tca8418::new(&mut chip, ADDRESS).expect("address");
        controller.initialize(7, 8).expect("initialize");
        let _ = controller.release();
        assert!(chip.fifo.is_empty());
        assert_eq!(chip.registers[0x02], 0);
        assert_eq!(&chip.registers[0x23..0x26], &[0, 0, 0]);
        assert_eq!(&chip.registers[0x20..0x23], &[0xFF; 3]);
        assert_eq!(&chip.registers[0x1A..0x1D], &[0xFF; 3]);
        assert_eq!(chip.registers[0x1D], 0x7F);
        assert_eq!(chip.registers[0x1E], 0xFF);
        // Eight columns leave KP_GPIO3 alone.
        assert!(!chip.writes.iter().any(|(register, _)| *register == 0x1F));
        assert_eq!(chip.registers[0x01], 0x03);
    }

    #[test]
    fn reads_events_until_the_fifo_is_empty() {
        let mut chip = Chip::default();
        // Row 3 column 5 pressed (key 36), then released.
        chip.fifo.extend([0x80 | 36, 36]);
        let mut controller = Tca8418::new(&mut chip, ADDRESS).expect("address");
        assert_eq!(controller.event_count(), Ok(2));
        let press = controller.read_event().expect("read").expect("event");
        assert!(press.is_pressed());
        assert_eq!(press.matrix_position(), Some((3, 5)));
        let release = controller.read_event().expect("read").expect("event");
        assert!(!release.is_pressed());
        assert_eq!(controller.read_event(), Ok(None));
        controller.clear_key_interrupt().expect("clear");
    }

    #[test]
    fn decodes_key_numbers() {
        assert_eq!(KeyEvent::from_register(0x81).map(KeyEvent::code), Some(1));
        assert_eq!(
            KeyEvent::from_register(0x81).and_then(KeyEvent::matrix_position),
            Some((0, 0))
        );
        assert_eq!(
            KeyEvent::from_register(80).and_then(KeyEvent::matrix_position),
            Some((7, 9))
        );
        // A GPI event is no matrix key.
        assert_eq!(
            KeyEvent::from_register(0x80 | 97).and_then(KeyEvent::matrix_position),
            None
        );
        assert_eq!(KeyEvent::from_register(0), None);
    }

    #[test]
    fn rejects_bad_configuration() {
        let mut chip = Chip::default();
        assert!(matches!(
            Tca8418::new(&mut chip, 0x80),
            Err(Error::InvalidAddress)
        ));
        let mut controller = Tca8418::new(&mut chip, ADDRESS).expect("address");
        assert_eq!(controller.initialize(9, 8), Err(Error::InvalidMatrix));
        assert_eq!(controller.initialize(7, 0), Err(Error::InvalidMatrix));
        controller.initialize(8, 10).expect("initialize");
        let _ = controller.release();
        assert_eq!(chip.registers[0x1F], 0x03);
    }
}

//! Low-level Driver for one PI4IOE5V6408 I2C digital I/O expander.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

const DIRECTION: u8 = 0x03;
const OUTPUT: u8 = 0x05;
const OUTPUT_HIGH_IMPEDANCE: u8 = 0x07;
const DEFAULT_STATE: u8 = 0x09;
const PULL_ENABLE: u8 = 0x0b;
const PULL_SELECTION: u8 = 0x0d;
const INPUT: u8 = 0x0f;
const INTERRUPT_MASK: u8 = 0x11;

/// Pull resistor applied to an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    /// No internal pull resistor.
    None,
    /// Internal pull-down resistor.
    Down,
    /// Internal pull-up resistor.
    Up,
}

/// PI4IOE5V6408 register or configuration failure.
#[derive(Debug)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The pin is outside the zero through seven range.
    InvalidPin,
    /// An I2C register operation failed.
    Bus(BusError),
}

/// Borrowed access to one PI4IOE5V6408 on a shared I2C bus.
pub struct Pi4ioe5v6408<'a, I2C> {
    i2c: &'a mut I2C,
    address: u8,
}

impl<'a, I2C> Pi4ioe5v6408<'a, I2C>
where
    I2C: I2c,
{
    /// Borrows one addressed expander.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidAddress`] for a non-seven-bit address.
    pub fn new(i2c: &'a mut I2C, address: u8) -> Result<Self, Error<I2C::Error>> {
        if address > 0x7f {
            return Err(Error::InvalidAddress);
        }
        Ok(Self { i2c, address })
    }

    /// Configures one push-pull output and establishes its initial level.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn configure_output(
        &mut self,
        pin: u8,
        initial_high: bool,
    ) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(pin)?;
        self.update(DIRECTION, |value| value | mask)?;
        self.update(OUTPUT_HIGH_IMPEDANCE, |value| value & !mask)?;
        self.update(OUTPUT, |value| set_bits(value, mask, initial_high))
    }

    /// Configures one input and its optional pull resistor.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn configure_input(&mut self, pin: u8, pull: Pull) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(pin)?;
        self.update(DIRECTION, |value| value & !mask)?;
        self.update(OUTPUT_HIGH_IMPEDANCE, |value| value | mask)?;
        self.update(PULL_ENABLE, |value| match pull {
            Pull::None => value & !mask,
            Pull::Down | Pull::Up => value | mask,
        })?;
        self.update(PULL_SELECTION, |value| match pull {
            Pull::Up => value | mask,
            Pull::None | Pull::Down => value & !mask,
        })
    }

    /// Drives one previously configured output.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_output(&mut self, pin: u8, high: bool) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(pin)?;
        self.update(OUTPUT, |value| set_bits(value, mask, high))
    }

    /// Reads one input level.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn input_is_high(&mut self, pin: u8) -> Result<bool, Error<I2C::Error>> {
        let mask = pin_mask(pin)?;
        Ok(self.read(INPUT)? & mask != 0)
    }

    /// Pulses an active-low reset output and leaves it released high.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn pulse_reset_low<D: DelayNs>(
        &mut self,
        pin: u8,
        low_ms: u32,
        settle_ms: u32,
        delay: &mut D,
    ) -> Result<(), Error<I2C::Error>> {
        self.configure_output(pin, false)?;
        delay.delay_ms(low_ms);
        self.set_output(pin, true)?;
        delay.delay_ms(settle_ms);
        Ok(())
    }

    /// Configures whether one input state participates in default-state comparison.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_default_state(&mut self, pin: u8, high: bool) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(pin)?;
        self.update(DEFAULT_STATE, |value| set_bits(value, mask, high))
    }

    /// Masks or unmasks interrupt generation for one pin.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_interrupt_masked(&mut self, pin: u8, masked: bool) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(pin)?;
        self.update(INTERRUPT_MASK, |value| set_bits(value, mask, masked))
    }

    fn update(
        &mut self,
        register: u8,
        update: impl FnOnce(u8) -> u8,
    ) -> Result<(), Error<I2C::Error>> {
        let value = self.read(register)?;
        self.write(register, update(value))
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

fn pin_mask<BusError>(pin: u8) -> Result<u8, Error<BusError>> {
    1_u8.checked_shl(u32::from(pin)).ok_or(Error::InvalidPin)
}

const fn set_bits(value: u8, mask: u8, high: bool) -> u8 {
    if high { value | mask } else { value & !mask }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{DIRECTION, OUTPUT, OUTPUT_HIGH_IMPEDANCE, PULL_ENABLE, PULL_SELECTION};
    use super::{Error, Pi4ioe5v6408, Pull};

    #[derive(Default)]
    struct Bus {
        registers: [u8; 0x12],
    }

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn read(&mut self, _address: u8, bytes: &mut [u8]) -> Result<(), Self::Error> {
            bytes.fill(0);
            Ok(())
        }

        fn write(&mut self, _address: u8, bytes: &[u8]) -> Result<(), Self::Error> {
            if let [register, value] = bytes {
                self.registers[usize::from(*register)] = *value;
            }
            Ok(())
        }

        fn write_read(
            &mut self,
            _address: u8,
            bytes: &[u8],
            read: &mut [u8],
        ) -> Result<(), Self::Error> {
            if let ([register], [value]) = (bytes, read) {
                *value = self.registers[usize::from(*register)];
            }
            Ok(())
        }

        fn transaction(
            &mut self,
            _address: u8,
            _operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay {
        elapsed_ms: u32,
    }

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.elapsed_ms = self.elapsed_ms.saturating_add(ns / 1_000_000);
        }
    }

    #[test]
    fn configures_output_without_overwriting_other_pins() {
        let mut bus = Bus::default();
        bus.registers[usize::from(DIRECTION)] = 0x80;
        bus.registers[usize::from(OUTPUT_HIGH_IMPEDANCE)] = 0xff;
        bus.registers[usize::from(OUTPUT)] = 0x40;
        {
            let mut expander = Pi4ioe5v6408::new(&mut bus, 0x43).expect("valid expander");
            expander
                .configure_output(4, true)
                .expect("configure output");
        }
        assert_eq!(bus.registers[usize::from(DIRECTION)], 0x90);
        assert_eq!(bus.registers[usize::from(OUTPUT_HIGH_IMPEDANCE)], 0xef);
        assert_eq!(bus.registers[usize::from(OUTPUT)], 0x50);
    }

    #[test]
    fn configures_input_pull_and_reset_pulse() {
        let mut bus = Bus::default();
        let mut delay = Delay::default();
        {
            let mut expander = Pi4ioe5v6408::new(&mut bus, 0x43).expect("valid expander");
            expander
                .configure_input(2, Pull::Up)
                .expect("configure input");
            expander
                .pulse_reset_low(6, 10, 20, &mut delay)
                .expect("pulse reset");
        }
        assert_eq!(bus.registers[usize::from(PULL_ENABLE)] & 0x04, 0x04);
        assert_eq!(bus.registers[usize::from(PULL_SELECTION)] & 0x04, 0x04);
        assert_eq!(bus.registers[usize::from(OUTPUT)] & 0x40, 0x40);
        assert_eq!(delay.elapsed_ms, 30);
    }

    #[test]
    fn rejects_invalid_addresses_and_pins() {
        let mut bus = Bus::default();
        assert!(matches!(
            Pi4ioe5v6408::new(&mut bus, 0x80),
            Err(Error::InvalidAddress)
        ));
        let mut expander = Pi4ioe5v6408::new(&mut bus, 0x43).expect("valid expander");
        assert!(matches!(
            expander.configure_output(8, false),
            Err(Error::InvalidPin)
        ));
    }
}

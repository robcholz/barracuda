//! Relay or transistor outputs and isolated inputs on AW9523B expander pins.
//!
//! The implementation owns only the pins the Board lists. At initialization
//! it checks the chip's ID, makes port 0 push-pull when an output sits there
//! (an open-drain pin cannot drive a relay driver's input high), latches every
//! output off before making it an output, makes the inputs inputs, and masks
//! the interrupts of all its pins so the shared INTN line stays quiet, as
//! M5Stack's StamPLC library does (`io_expander_b_init`). Each register
//! change is a read-modify-write of the pin's own bit.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_aw9523::{Aw9523, Error as ExpanderError};
use barracuda_peripheral::{
    PeripheralImplementation,
    digital_io::{DigitalIo, Levels, MAX_CHANNELS},
};
use embedded_hal::i2c::I2c;

/// The level that switches an output on, or that an energized input puts on
/// its pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveLevel {
    /// High is on.
    High,
    /// Low is on.
    Low,
}

impl ActiveLevel {
    const fn level(self, on: bool) -> bool {
        match self {
            Self::High => on,
            Self::Low => !on,
        }
    }
}

/// Board-owned expander address, pins, and polarity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aw9523DigitalIoConfig {
    address: u8,
    output_pins: u16,
    input_pins: u16,
    output_active: ActiveLevel,
    input_active: ActiveLevel,
}

impl Aw9523DigitalIoConfig {
    /// Creates a configuration from the expander address, the output and
    /// input pins (bit `n` is expander pin `n`: P0_0 through P1_7), and the
    /// active level of each side.
    #[must_use]
    pub const fn new(
        address: u8,
        output_pins: u16,
        input_pins: u16,
        output_active: ActiveLevel,
        input_active: ActiveLevel,
    ) -> Self {
        Self {
            address,
            output_pins,
            input_pins,
            output_active,
            input_active,
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct Aw9523DigitalIoBindings<I2C> {
    i2c: I2C,
}

impl<I2C> Aw9523DigitalIoBindings<I2C> {
    /// Wraps the expander's I2C device view.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// Initialization or transfer failure.
#[derive(Debug)]
pub enum Aw9523DigitalIoError<E> {
    /// The Board named one pin as both output and input.
    OverlappingPins,
    /// The device at the address is not an AW9523B.
    NotPresent,
    /// The channel index is past the last output.
    NoSuchOutput(usize),
    /// An expander register operation failed.
    Expander(ExpanderError<E>),
}

impl<E> From<ExpanderError<E>> for Aw9523DigitalIoError<E> {
    fn from(error: ExpanderError<E>) -> Self {
        Self::Expander(error)
    }
}

/// The expander pins of one side, in channel order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pins {
    pins: [u8; MAX_CHANNELS],
    count: usize,
}

impl Pins {
    fn from_mask(mask: u16) -> Self {
        let mut pins = [0; MAX_CHANNELS];
        let mut count: usize = 0;
        for pin in 0..16_u8 {
            if mask.checked_shr(u32::from(pin)).unwrap_or_default() & 1 != 0
                && let Some(slot) = pins.get_mut(count)
            {
                *slot = pin;
                count = count.saturating_add(1);
            }
        }
        Self { pins, count }
    }

    fn as_slice(&self) -> &[u8] {
        self.pins.get(..self.count).unwrap_or(&[])
    }
}

/// Initialized digital I/O bank.
pub struct Aw9523DigitalIo<I2C> {
    i2c: I2C,
    address: u8,
    outputs: Pins,
    inputs: Pins,
    output_active: ActiveLevel,
    input_active: ActiveLevel,
    states: Levels,
}

/// Static factory used by generated Board composition.
pub struct Aw9523DigitalIoImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for Aw9523DigitalIoImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = Aw9523DigitalIoBindings<I2C>;
    type Config = Aw9523DigitalIoConfig;
    type Peripheral = Aw9523DigitalIo<I2C>;
    type Error = Aw9523DigitalIoError<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        if config.output_pins & config.input_pins != 0 {
            return Err(Aw9523DigitalIoError::OverlappingPins);
        }
        let outputs = Pins::from_mask(config.output_pins);
        let inputs = Pins::from_mask(config.input_pins);
        let mut i2c = bindings.i2c;
        let mut expander = Aw9523::new(&mut i2c, config.address)?;
        if !expander.is_present()? {
            return Err(Aw9523DigitalIoError::NotPresent);
        }
        if config.output_pins & 0x00ff != 0 {
            expander.set_port0_push_pull(true)?;
        }
        for &pin in outputs.as_slice() {
            expander.configure_output(pin, config.output_active.level(false))?;
            expander.set_interrupt_enabled(pin, false)?;
        }
        for &pin in inputs.as_slice() {
            expander.configure_input(pin)?;
            expander.set_interrupt_enabled(pin, false)?;
        }
        Ok(Aw9523DigitalIo {
            i2c,
            address: config.address,
            outputs,
            inputs,
            output_active: config.output_active,
            input_active: config.input_active,
            states: Levels::default(),
        })
    }
}

impl<I2C: I2c> DigitalIo for Aw9523DigitalIo<I2C> {
    type Error = Aw9523DigitalIoError<I2C::Error>;

    fn output_count(&self) -> usize {
        self.outputs.count
    }

    fn input_count(&self) -> usize {
        self.inputs.count
    }

    fn set_output(&mut self, index: usize, on: bool) -> Result<(), Self::Error> {
        let pin = *self
            .outputs
            .as_slice()
            .get(index)
            .ok_or(Aw9523DigitalIoError::NoSuchOutput(index))?;
        Aw9523::new(&mut self.i2c, self.address)?.set_output(pin, self.output_active.level(on))?;
        self.states = self.states.with(index, on);
        Ok(())
    }

    fn outputs(&self) -> Levels {
        self.states
    }

    fn read_inputs(&mut self) -> Result<Levels, Self::Error> {
        let raw = Aw9523::new(&mut self.i2c, self.address)?.read_inputs()?;
        let mut levels = Levels::default();
        for (index, &pin) in self.inputs.as_slice().iter().enumerate() {
            let high = raw.checked_shr(u32::from(pin)).unwrap_or_default() & 1 != 0;
            levels = levels.with(index, high == self.input_active.level(true));
        }
        Ok(levels)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use barracuda_peripheral::{PeripheralImplementation, digital_io::DigitalIo};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{
        ActiveLevel, Aw9523DigitalIoBindings, Aw9523DigitalIoConfig, Aw9523DigitalIoError,
        Aw9523DigitalIoImplementation,
    };

    const ADDRESS: u8 = 0x59;

    /// A register-file model of the AW9523B at its power-on state.
    struct Expander {
        registers: [u8; 0x80],
        writes: Vec<(u8, u8)>,
    }

    impl Expander {
        fn new() -> Self {
            let mut registers = [0; 0x80];
            registers[0x10] = 0x23;
            // Mode: every pin GPIO.
            registers[0x12] = 0xff;
            registers[0x13] = 0xff;
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
            assert_eq!(address, ADDRESS);
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

    /// M5Stack StamPLC: relays on P0_0-P0_3, inputs on P0_4-P0_7 and
    /// P1_4-P1_7 (M5StamPLC `_out_pin_list`, `_in_pin_list`).
    fn stamp_plc() -> Aw9523DigitalIoConfig {
        Aw9523DigitalIoConfig::new(
            ADDRESS,
            0x000f,
            0xf0f0,
            ActiveLevel::High,
            ActiveLevel::High,
        )
    }

    #[test]
    fn configures_relays_off_and_inputs_with_interrupts_masked() {
        let io = block_on(Aw9523DigitalIoImplementation::initialize(
            Aw9523DigitalIoBindings::new(Expander::new()),
            stamp_plc(),
        ))
        .expect("initialize");
        assert_eq!(io.output_count(), 4);
        assert_eq!(io.input_count(), 8);
        let registers = io.i2c.registers;
        // Port 0 push-pull, outputs latched low, directions, interrupts.
        assert_eq!(registers[0x11] & 0x10, 0x10);
        assert_eq!(registers[0x02] & 0x0f, 0x00);
        assert_eq!(registers[0x04], 0xf0);
        assert_eq!(registers[0x05], 0xf0);
        assert_eq!(registers[0x06], 0xff);
        assert_eq!(registers[0x07], 0xf0);
        // P1_0-P1_3 belong to others and keep their direction and mask.
        assert_eq!(registers[0x05] & 0x0f, 0x00);
    }

    #[test]
    fn switches_relays_and_reads_inputs_in_label_order() {
        let mut io = block_on(Aw9523DigitalIoImplementation::initialize(
            Aw9523DigitalIoBindings::new(Expander::new()),
            stamp_plc(),
        ))
        .expect("initialize");
        io.set_output(2, true).expect("relay 3 on");
        assert_eq!(io.i2c.registers[0x02], 0x04);
        assert!(io.outputs().is_on(2));
        io.set_output(2, false).expect("relay 3 off");
        assert_eq!(io.i2c.registers[0x02], 0x00);
        assert!(matches!(
            io.set_output(4, true),
            Err(Aw9523DigitalIoError::NoSuchOutput(4))
        ));
        // Inputs 1 (P0_4) and 8 (P1_7) energized.
        io.i2c.registers[0x00] = 0x10;
        io.i2c.registers[0x01] = 0x80;
        let inputs = io.read_inputs().expect("read");
        assert_eq!(inputs.bits(), 0b1000_0001);
    }

    #[test]
    fn active_low_outputs_latch_high_and_overlaps_are_refused() {
        let io = block_on(Aw9523DigitalIoImplementation::initialize(
            Aw9523DigitalIoBindings::new(Expander::new()),
            Aw9523DigitalIoConfig::new(ADDRESS, 0x0300, 0, ActiveLevel::Low, ActiveLevel::High),
        ))
        .expect("initialize");
        assert_eq!(io.i2c.registers[0x03], 0x03);
        // No port 0 output: its drive mode is left alone.
        assert_eq!(io.i2c.registers[0x11], 0x00);
        let result = block_on(Aw9523DigitalIoImplementation::initialize(
            Aw9523DigitalIoBindings::new(Expander::new()),
            Aw9523DigitalIoConfig::new(
                ADDRESS,
                0x0011,
                0x0010,
                ActiveLevel::High,
                ActiveLevel::High,
            ),
        ));
        assert!(matches!(result, Err(Aw9523DigitalIoError::OverlappingPins)));
    }

    #[test]
    fn refuses_another_chip() {
        let mut expander = Expander::new();
        expander.registers[0x10] = 0x00;
        let result = block_on(Aw9523DigitalIoImplementation::initialize(
            Aw9523DigitalIoBindings::new(expander),
            stamp_plc(),
        ));
        assert!(matches!(result, Err(Aw9523DigitalIoError::NotPresent)));
    }
}

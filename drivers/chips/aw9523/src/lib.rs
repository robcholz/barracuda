//! Low-level Driver for one Awinic AW9523B I2C 16-bit I/O expander.
//!
//! Register layout follows the AW9523B datasheet: REG00H/01H inputs,
//! REG02H/03H outputs, REG04H/05H direction (1 = input), REG06H/07H interrupt
//! enable (1 = disabled), REG10H ID (23h), REG11H global control (bit 4
//! selects push-pull for port 0), and REG12H/13H mode switch (1 = GPIO,
//! 0 = LED current sink).
//!
//! Pins are numbered 0 through 15: P0_0 through P0_7 are 0 through 7, and
//! P1_0 through P1_7 are 8 through 15.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

const INPUT_P0: u8 = 0x00;
const OUTPUT_P0: u8 = 0x02;
const CONFIG_P0: u8 = 0x04;
const INTERRUPT_P0: u8 = 0x06;
const ID: u8 = 0x10;
const ID_AW9523B: u8 = 0x23;
const GLOBAL_CONTROL: u8 = 0x11;
const MODE_P0: u8 = 0x12;

/// REG11H bit 4: port 0 push-pull (1) or open-drain (0); port 1 is always push-pull.
const PORT0_PUSH_PULL: u8 = 1 << 4;

/// AW9523B register or configuration failure.
#[derive(Debug)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The pin is outside the zero through fifteen range.
    InvalidPin,
    /// An I2C register operation failed.
    Bus(BusError),
}

/// Borrowed access to one AW9523B on a shared I2C bus.
pub struct Aw9523<'a, I2C> {
    i2c: &'a mut I2C,
    address: u8,
}

impl<'a, I2C> Aw9523<'a, I2C>
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

    /// Reads the ID register and reports whether it identifies an AW9523B.
    ///
    /// # Errors
    ///
    /// Returns an I2C error when no device acknowledges the read.
    pub fn is_present(&mut self) -> Result<bool, Error<I2C::Error>> {
        Ok(self.read(ID)? == ID_AW9523B)
    }

    /// Selects push-pull (`true`) or open-drain (`false`) outputs on port 0.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_port0_push_pull(&mut self, push_pull: bool) -> Result<(), Error<I2C::Error>> {
        self.update(GLOBAL_CONTROL, |value| {
            set_bits(value, PORT0_PUSH_PULL, push_pull)
        })
    }

    /// Configures one pin as a GPIO output and establishes its initial level.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn configure_output(
        &mut self,
        pin: u8,
        initial_high: bool,
    ) -> Result<(), Error<I2C::Error>> {
        let (offset, mask) = pin_location(pin)?;
        // Latch the level before the pin becomes an output so it never glitches.
        self.update(OUTPUT_P0 | offset, |value| {
            set_bits(value, mask, initial_high)
        })?;
        self.update(MODE_P0 | offset, |value| value | mask)?;
        self.update(CONFIG_P0 | offset, |value| value & !mask)
    }

    /// Configures one pin as a GPIO input.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn configure_input(&mut self, pin: u8) -> Result<(), Error<I2C::Error>> {
        let (offset, mask) = pin_location(pin)?;
        self.update(MODE_P0 | offset, |value| value | mask)?;
        self.update(CONFIG_P0 | offset, |value| value | mask)
    }

    /// Drives one previously configured output.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_output(&mut self, pin: u8, high: bool) -> Result<(), Error<I2C::Error>> {
        let (offset, mask) = pin_location(pin)?;
        self.update(OUTPUT_P0 | offset, |value| set_bits(value, mask, high))
    }

    /// Reads one input level. Reading a port also clears its pending interrupt.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn input_is_high(&mut self, pin: u8) -> Result<bool, Error<I2C::Error>> {
        let (offset, mask) = pin_location(pin)?;
        Ok(self.read(INPUT_P0 | offset)? & mask != 0)
    }

    /// Reads both input ports at once: P0 in the low byte, P1 in the high
    /// byte. Reading a port also clears its pending interrupt.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn read_inputs(&mut self) -> Result<u16, Error<I2C::Error>> {
        let low = self.read(INPUT_P0)?;
        let high = self.read(INPUT_P0 | 1)?;
        Ok(u16::from_le_bytes([low, high]))
    }

    /// Returns whether one pin is configured as an input.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn is_input(&mut self, pin: u8) -> Result<bool, Error<I2C::Error>> {
        let (offset, mask) = pin_location(pin)?;
        Ok(self.read(CONFIG_P0 | offset)? & mask != 0)
    }

    /// Enables or disables the interrupt a level change of one input raises
    /// on the shared INTN line.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_interrupt_enabled(
        &mut self,
        pin: u8,
        enabled: bool,
    ) -> Result<(), Error<I2C::Error>> {
        let (offset, mask) = pin_location(pin)?;
        // A set bit masks the pin's interrupt.
        self.update(INTERRUPT_P0 | offset, |value| {
            set_bits(value, mask, !enabled)
        })
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

/// Returns the port register offset (0 for port 0, 1 for port 1) and the bit
/// mask of one pin.
fn pin_location<BusError>(pin: u8) -> Result<(u8, u8), Error<BusError>> {
    if pin > 15 {
        return Err(Error::InvalidPin);
    }
    let offset = u8::from(pin >= 8);
    let bit = pin & 0x07;
    let mask = 1_u8.checked_shl(u32::from(bit)).ok_or(Error::InvalidPin)?;
    Ok((offset, mask))
}

const fn set_bits(value: u8, mask: u8, high: bool) -> u8 {
    if high { value | mask } else { value & !mask }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{Aw9523, CONFIG_P0, Error, GLOBAL_CONTROL, INTERRUPT_P0, MODE_P0, OUTPUT_P0};

    const ADDRESS: u8 = 0x58;

    struct Bus {
        registers: [u8; 0x80],
        writes: std::vec::Vec<(u8, u8)>,
    }

    impl Default for Bus {
        fn default() -> Self {
            let mut registers = [0; 0x80];
            registers[0x10] = 0x23;
            registers[usize::from(MODE_P0)] = 0xff;
            registers[usize::from(MODE_P0) + 1] = 0xff;
            Self {
                registers,
                writes: std::vec::Vec::new(),
            }
        }
    }

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, ADDRESS);
            let mut register = None;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = Some(*reg),
                    Operation::Write([reg, value]) => {
                        self.registers[usize::from(*reg)] = *value;
                        self.writes.push((*reg, *value));
                    }
                    Operation::Read([value]) => {
                        *value = self.registers[usize::from(register.expect("register"))];
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay {
        elapsed_ms: u32,
    }

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.elapsed_ms += ns / 1_000_000;
        }
    }

    #[test]
    fn identifies_aw9523b() {
        let mut bus = Bus::default();
        assert!(
            Aw9523::new(&mut bus, ADDRESS)
                .expect("valid address")
                .is_present()
                .expect("read")
        );
        bus.registers[0x10] = 0x00;
        assert!(
            !Aw9523::new(&mut bus, ADDRESS)
                .expect("valid address")
                .is_present()
                .expect("read")
        );
    }

    #[test]
    fn configures_port1_output_level_before_direction() {
        let mut bus = Bus::default();
        bus.registers[usize::from(CONFIG_P0) + 1] = 0x0e;
        bus.registers[usize::from(MODE_P0) + 1] = 0xfd;
        bus.registers[usize::from(OUTPUT_P0) + 1] = 0x81;
        {
            let mut expander = Aw9523::new(&mut bus, ADDRESS).expect("valid address");
            // M5Stack CoreS3: LCD_RST on P1_1 (pin 9).
            expander.configure_output(9, true).expect("configure");
        }
        assert_eq!(bus.registers[usize::from(OUTPUT_P0) + 1], 0x83);
        assert_eq!(bus.registers[usize::from(MODE_P0) + 1], 0xff);
        assert_eq!(bus.registers[usize::from(CONFIG_P0) + 1], 0x0c);
        // Port 0 registers are untouched.
        assert_eq!(bus.registers[usize::from(OUTPUT_P0)], 0x00);
        let order: std::vec::Vec<u8> = bus.writes.iter().map(|(register, _)| *register).collect();
        assert_eq!(order, [0x03, 0x13, 0x05]);
    }

    #[test]
    fn pulses_reset_and_selects_port0_drive() {
        let mut bus = Bus::default();
        let mut delay = Delay::default();
        {
            let mut expander = Aw9523::new(&mut bus, ADDRESS).expect("valid address");
            expander.set_port0_push_pull(true).expect("push-pull");
            expander
                .pulse_reset_low(9, 10, 120, &mut delay)
                .expect("pulse");
            expander.configure_input(3).expect("input");
        }
        assert_eq!(bus.registers[usize::from(GLOBAL_CONTROL)], 0x10);
        assert_eq!(bus.registers[usize::from(OUTPUT_P0) + 1], 0x02);
        assert_eq!(bus.registers[usize::from(CONFIG_P0)], 0x08);
        assert_eq!(delay.elapsed_ms, 130);
    }

    #[test]
    fn reads_inputs_and_rejects_invalid_pins() {
        let mut bus = Bus::default();
        bus.registers[0x01] = 0x04;
        let mut expander = Aw9523::new(&mut bus, ADDRESS).expect("valid address");
        assert!(expander.input_is_high(10).expect("read"));
        assert!(!expander.input_is_high(2).expect("read"));
        assert!(matches!(
            expander.configure_output(16, false),
            Err(Error::InvalidPin)
        ));
        assert!(matches!(
            Aw9523::new(&mut bus, 0x80),
            Err(Error::InvalidAddress)
        ));
    }

    #[test]
    fn reads_both_ports_and_masks_interrupts() {
        let mut bus = Bus::default();
        bus.registers[0x00] = 0xa0;
        bus.registers[0x01] = 0x50;
        {
            let mut expander = Aw9523::new(&mut bus, ADDRESS).expect("valid address");
            assert_eq!(expander.read_inputs().expect("read"), 0x50a0);
            expander.configure_input(12).expect("input");
            assert!(expander.is_input(12).expect("direction"));
            assert!(!expander.is_input(0).expect("direction"));
            expander.set_interrupt_enabled(12, false).expect("mask");
            expander.set_interrupt_enabled(4, true).expect("unmask");
        }
        assert_eq!(bus.registers[usize::from(INTERRUPT_P0) + 1], 0x10);
        assert_eq!(bus.registers[usize::from(INTERRUPT_P0)], 0x00);
    }
}

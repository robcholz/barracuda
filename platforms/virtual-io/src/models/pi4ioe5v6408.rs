//! Diodes PI4IOE5V6408 8-bit I2C I/O expander.
//!
//! Source: Diodes datasheet DS40583 Rev 3-5 (September 2022),
//! "PI4IOE5V6408 Low-voltage Translating 8-bit I2C-bus I/O Expander".
//! Section letters and table numbers below refer to that revision.
//!
//! Modelled: the register map and power-up defaults (b. Table 2), the Device
//! ID and Control register with its reset-interrupt and software-reset bits
//! (c.i Table 3), how direction, output state, high-impedance, and pull
//! registers decide each pin (c.ii–c.vii, d.), the Input Status register
//! (c.viii), the single-register read procedure ("I2C Read /Write
//! Procedures"), and the interrupt status latch (c.x). Not modelled: the INT
//! pin, the RESET pin, and bus timing.
//!
//! Every change of a pin's driven state is noted in the hardware event log as
//! `P<n> high`, `P<n> low`, or `P<n> released`, so a test can check that a
//! driver never glitches an output.
//!
//! Backdoor layout: byte offset equals the register address (00h–13h);
//! offset 20h holds the externally driven levels and 21h the mask of pins
//! driven from outside. Pins neither driven nor pulled read low.

use crate::device::{DataNack, DeviceContext, I2cDevice, RegisterRangeError};

const DEVICE_ID: u8 = 0x01;
const DIRECTION: u8 = 0x03;
const OUTPUT: u8 = 0x05;
const HIGH_IMPEDANCE: u8 = 0x07;
const DEFAULT_STATE: u8 = 0x09;
const PULL_ENABLE: u8 = 0x0b;
const PULL_SELECT: u8 = 0x0d;
const INPUT_STATUS: u8 = 0x0f;
const INTERRUPT_MASK: u8 = 0x11;
const INTERRUPT_STATUS: u8 = 0x13;
const EXTERNAL_LEVEL: usize = 0x20;
const EXTERNAL_MASK: usize = 0x21;
const BACKDOOR_BYTES: usize = 0x22;

/// Table 3: manufacturer ID 101b, firmware revision 000b, reset interrupt 1.
const DEVICE_ID_POR: u8 = 0b1010_0010;
/// Table 3: B1, reset interrupt.
const RESET_INTERRUPT: u8 = 1 << 1;
/// Table 3: B0, software reset.
const SOFTWARE_RESET: u8 = 1 << 0;

/// Rule identifiers reported by this model.
pub mod rules {
    /// Table 2: the command byte names a register of the register map.
    pub const REGISTER_MAP: &str = "PI4IOE5V6408 DS40583 Table 2 register map";
    /// "I2C Read /Write Procedures": burst reads are not supported.
    pub const BURST_READ: &str = "PI4IOE5V6408 DS40583 I2C procedures burst read";
    /// The datasheet describes one data byte per write; more are undefined.
    pub const BURST_WRITE: &str = "PI4IOE5V6408 DS40583 I2C procedures multi-byte write";
}

/// Driven state of one expander pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drive {
    High,
    Low,
    Released,
}

/// Behavioural model of one PI4IOE5V6408.
#[derive(Clone, Debug)]
pub struct Pi4ioe5v6408Model {
    pointer: u8,
    device_id: u8,
    direction: u8,
    output: u8,
    high_impedance: u8,
    default_state: u8,
    pull_enable: u8,
    pull_select: u8,
    interrupt_mask: u8,
    interrupt_status: u8,
    external_level: u8,
    external_mask: u8,
    /// Inputs that were in their non-default state at the last latch.
    non_default: u8,
    drives: [Drive; 8],
}

impl Pi4ioe5v6408Model {
    /// Creates a device in its power-up state (Table 2).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pointer: 0,
            device_id: DEVICE_ID_POR,
            direction: 0x00,
            output: 0x00,
            high_impedance: 0xff,
            default_state: 0x00,
            pull_enable: 0xff,
            pull_select: 0x00,
            interrupt_mask: 0x00,
            interrupt_status: 0x00,
            external_level: 0,
            external_mask: 0,
            non_default: 0,
            drives: [Drive::Released; 8],
        }
    }

    fn reset(&mut self) {
        let (level, mask) = (self.external_level, self.external_mask);
        *self = Self::new();
        self.external_level = level;
        self.external_mask = mask;
    }

    /// c.ii, c.iii, c.iv: a pin drives the Output State only when it is an
    /// output and its high-impedance bit is 0.
    fn drive(&self, pin: usize) -> Drive {
        let mask = 1 << pin;
        if self.direction & mask == 0 || self.high_impedance & mask != 0 {
            Drive::Released
        } else if self.output & mask != 0 {
            Drive::High
        } else {
            Drive::Low
        }
    }

    /// c.viii: input levels; outputs read low. d.: pulls apply to inputs.
    fn input_status(&self) -> u8 {
        (0..8).fold(0, |status, pin| {
            let mask = 1 << pin;
            let level = if self.direction & mask != 0 {
                false
            } else if self.external_mask & mask != 0 {
                self.external_level & mask != 0
            } else {
                self.pull_enable & mask != 0 && self.pull_select & mask != 0
            };
            if level {
                status | mask
            } else {
                status
            }
        })
    }

    fn note_drives(&mut self, context: &mut DeviceContext<'_>) {
        for pin in 0..8 {
            let drive = self.drive(pin);
            if drive != self.drives[pin] {
                self.drives[pin] = drive;
                let state = match drive {
                    Drive::High => "high",
                    Drive::Low => "low",
                    Drive::Released => "released",
                };
                context.note(format!("P{pin} {state}"));
            }
        }
    }

    fn register(&self, register: u8) -> Option<u8> {
        Some(match register {
            // Table 3: B0 always reads 0.
            DEVICE_ID => self.device_id & !SOFTWARE_RESET,
            DIRECTION => self.direction,
            OUTPUT => self.output,
            HIGH_IMPEDANCE => self.high_impedance,
            DEFAULT_STATE => self.default_state,
            PULL_ENABLE => self.pull_enable,
            PULL_SELECT => self.pull_select,
            INPUT_STATUS => self.input_status(),
            INTERRUPT_MASK => self.interrupt_mask,
            INTERRUPT_STATUS => self.interrupt_status,
            // Table 2: reserved registers are listed as R/W.
            0x02 | 0x04 | 0x06 | 0x08 | 0x0a | 0x0c | 0x0e | 0x10 | 0x12 => 0,
            _ => return None,
        })
    }

    fn store(&mut self, register: u8, value: u8) {
        match register {
            DEVICE_ID if value & SOFTWARE_RESET != 0 => {
                // g. Software Reset: registers return to their defaults, and
                // c.i: the reset interrupt is set by a software reset.
                self.reset();
            }
            DIRECTION => self.direction = value,
            OUTPUT => self.output = value,
            HIGH_IMPEDANCE => self.high_impedance = value,
            DEFAULT_STATE => self.default_state = value,
            PULL_ENABLE => self.pull_enable = value,
            PULL_SELECT => self.pull_select = value,
            INTERRUPT_MASK => self.interrupt_mask = value,
            // c.viii: writes to Input Status have no effect; other registers
            // keep their documented behaviour.
            _ => {}
        }
    }

    /// c.x: an input bit is latched when it changes to the value opposite
    /// its default state, and is not set again until the input has returned
    /// to the default state; output pins never set it.
    fn latch_interrupts(&mut self) {
        let non_default = (self.input_status() ^ self.default_state) & !self.direction;
        self.interrupt_status |= non_default & !self.non_default;
        self.non_default = non_default;
    }
}

impl Default for Pi4ioe5v6408Model {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Pi4ioe5v6408Model {
    fn model(&self) -> &'static str {
        "pi4ioe5v6408"
    }

    fn write(&mut self, context: &mut DeviceContext<'_>, bytes: &[u8]) -> Result<(), DataNack> {
        let Some((&register, data)) = bytes.split_first() else {
            return Ok(());
        };
        if self.register(register).is_none() {
            context.violation(
                rules::REGISTER_MAP,
                format!("command byte {register:#04x} is not in the register map"),
            );
        }
        self.pointer = register;
        if data.len() > 1 {
            context.violation(
                rules::BURST_WRITE,
                format!(
                    "write of {} data bytes to {register:#04x}; the model stores the first",
                    data.len()
                ),
            );
        }
        if let Some(&value) = data.first() {
            self.store(register, value);
            self.latch_interrupts();
            self.note_drives(context);
        }
        Ok(())
    }

    fn read(&mut self, context: &mut DeviceContext<'_>, buffer: &mut [u8]) {
        if buffer.len() > 1 {
            context.violation(
                rules::BURST_READ,
                format!("read of {} bytes from {:#04x}", buffer.len(), self.pointer),
            );
        }
        self.latch_interrupts();
        let value = self.register(self.pointer).unwrap_or(0);
        buffer.fill(value);
        match self.pointer {
            // c.i: the reset interrupt clears after it is read.
            DEVICE_ID => self.device_id &= !RESET_INTERRUPT,
            // c.x: reading the Interrupt Status register clears it.
            INTERRUPT_STATUS => self.interrupt_status = 0,
            _ => {}
        }
    }

    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
        let mut bytes = [0; BACKDOOR_BYTES];
        for register in 0..=INTERRUPT_STATUS {
            bytes[usize::from(register)] = self.register(register).unwrap_or(0);
        }
        bytes[EXTERNAL_LEVEL] = self.external_level;
        bytes[EXTERNAL_MASK] = self.external_mask;
        let source = crate::device::register_range(&bytes, offset, buffer.len())?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn poke(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RegisterRangeError> {
        let length = bytes.len();
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= BACKDOOR_BYTES)
            .ok_or(RegisterRangeError { offset, length })?;
        for (index, &value) in (offset..end).zip(bytes) {
            match index {
                EXTERNAL_LEVEL => self.external_level = value,
                EXTERNAL_MASK => self.external_mask = value,
                _ => {
                    if let Ok(register) = u8::try_from(index) {
                        self.store(register, value);
                    }
                }
            }
        }
        self.latch_interrupts();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::i2c::I2c as _;

    use super::*;
    use crate::{Clock, EventDetail, VirtualHardware};

    const ADDRESS: u8 = 0x43;

    fn setup() -> (VirtualHardware, crate::VirtualI2cBus) {
        let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
        hardware
            .attach(
                "I2C0",
                u64::from(ADDRESS),
                Box::new(Pi4ioe5v6408Model::new()),
            )
            .expect("attach");
        (hardware.clone(), hardware.i2c_bus("I2C0").expect("bus"))
    }

    fn read(bus: &mut crate::VirtualI2cBus, register: u8) -> u8 {
        let mut value = [0];
        bus.write_read(ADDRESS, &[register], &mut value)
            .expect("read");
        value[0]
    }

    fn notes(hardware: &VirtualHardware) -> Vec<String> {
        hardware
            .events(0)
            .into_iter()
            .filter_map(|event| match event.detail {
                EventDetail::Device { note, .. } => Some(note),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn power_up_defaults_follow_table_2_and_the_reset_interrupt_clears() {
        let (hardware, mut bus) = setup();
        assert_eq!(read(&mut bus, DEVICE_ID), 0xa2);
        assert_eq!(read(&mut bus, DEVICE_ID), 0xa0);
        assert_eq!(read(&mut bus, HIGH_IMPEDANCE), 0xff);
        assert_eq!(read(&mut bus, PULL_ENABLE), 0xff);
        assert_eq!(
            read(&mut bus, INPUT_STATUS),
            0x00,
            "pulled down at power-up"
        );
        assert!(hardware.violations().is_empty());
    }

    #[test]
    fn a_pin_drives_only_as_an_enabled_output() {
        let (hardware, mut bus) = setup();
        bus.write(ADDRESS, &[OUTPUT, 0x08]).expect("latch");
        bus.write(ADDRESS, &[DIRECTION, 0x08]).expect("direction");
        assert!(notes(&hardware).is_empty(), "still high-impedance");
        bus.write(ADDRESS, &[HIGH_IMPEDANCE, 0xf7]).expect("enable");
        bus.write(ADDRESS, &[OUTPUT, 0x00]).expect("low");
        assert_eq!(notes(&hardware), ["P3 high", "P3 low"]);
        assert_eq!(read(&mut bus, INPUT_STATUS) & 0x08, 0, "outputs read low");
    }

    #[test]
    fn inputs_follow_external_levels_and_pulls() {
        let (hardware, mut bus) = setup();
        bus.write(ADDRESS, &[PULL_SELECT, 0x01])
            .expect("pull-up P0");
        assert_eq!(read(&mut bus, INPUT_STATUS), 0x01);
        assert_eq!(
            read(&mut bus, INTERRUPT_STATUS),
            0x01,
            "left the low default"
        );
        assert_eq!(
            read(&mut bus, INTERRUPT_STATUS),
            0x00,
            "not set again while high"
        );
        hardware
            .write_registers("I2C0", u64::from(ADDRESS), EXTERNAL_LEVEL, &[0x00, 0x01])
            .expect("drive P0 low");
        assert_eq!(read(&mut bus, INPUT_STATUS), 0x00);
        hardware
            .write_registers("I2C0", u64::from(ADDRESS), EXTERNAL_LEVEL, &[0x01])
            .expect("drive P0 high");
        assert_eq!(read(&mut bus, INTERRUPT_STATUS), 0x01);
        assert_eq!(
            read(&mut bus, INTERRUPT_STATUS),
            0x00,
            "cleared by the read"
        );
    }

    #[test]
    fn software_reset_restores_defaults() {
        let (hardware, mut bus) = setup();
        bus.write(ADDRESS, &[DIRECTION, 0xff]).expect("direction");
        let _ = read(&mut bus, DEVICE_ID);
        bus.write(ADDRESS, &[DEVICE_ID, 0x01])
            .expect("software reset");
        assert_eq!(read(&mut bus, DIRECTION), 0x00);
        assert_eq!(read(&mut bus, DEVICE_ID), 0xa2);
        assert!(hardware.violations().is_empty());
    }

    #[test]
    fn unsupported_transfers_are_reported() {
        let (hardware, mut bus) = setup();
        let mut two = [0; 2];
        bus.write_read(ADDRESS, &[DIRECTION], &mut two)
            .expect("burst read");
        bus.write(ADDRESS, &[DIRECTION, 1, 2]).expect("burst write");
        bus.write(ADDRESS, &[0x20, 0]).expect("unknown register");
        let rules: Vec<_> = hardware
            .violations()
            .into_iter()
            .map(|violation| violation.violation.rule)
            .collect();
        assert_eq!(
            rules,
            [rules::BURST_READ, rules::BURST_WRITE, rules::REGISTER_MAP]
        );
    }
}

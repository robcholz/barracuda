//! Byte-register devices whose datasheet gives a register map, power-on
//! defaults, and a one-register I2C write and read format.
//!
//! A [`RegisterMapSpec`] carries a chip's documented registers with their
//! defaults and the citations the rules report; [`RegisterMapModel`] applies
//! the rules every such datasheet states:
//!
//! - the register address names a documented register;
//! - a write carries the register address and one data byte;
//! - a read is preceded by a write of the register address in the same
//!   transaction ("to read data from a register, you must set R/W bit to 0 to
//!   access the register address and then set R/W to 1 to read data").
//!
//! Writes store the value; registers marked reserved are stored without a
//! violation, because the datasheets do not state that writing them is
//! forbidden.
//!
//! Backdoor layout: byte offset equals the register address (00h–FFh).

use crate::device::{DataNack, DeviceContext, I2cDevice, RegisterRangeError};

/// Documented register set and rule citations of one chip.
#[derive(Debug)]
pub struct RegisterMapSpec {
    /// Model name used by the control interface.
    pub model: &'static str,
    /// Documented registers and their power-on defaults.
    pub registers: &'static [(u8, u8)],
    /// Registers the datasheet lists as reserved (default 0 in the model).
    pub reserved: &'static [u8],
    /// Rule identifier for an address outside the documented map.
    pub map_rule: &'static str,
    /// Rule identifier for a write that is not address plus one byte.
    pub write_rule: &'static str,
    /// Rule identifier for a read without an address write before it.
    pub read_rule: &'static str,
}

/// Behavioural model of one register-map device.
#[derive(Clone, Debug)]
pub struct RegisterMapModel {
    spec: &'static RegisterMapSpec,
    registers: [u8; 256],
    pointer: u8,
    addressed: bool,
}

impl RegisterMapModel {
    /// Creates a device with the power-on defaults of `spec`.
    #[must_use]
    pub fn new(spec: &'static RegisterMapSpec) -> Self {
        let mut registers = [0; 256];
        for &(register, value) in spec.registers {
            registers[usize::from(register)] = value;
        }
        Self {
            spec,
            registers,
            pointer: 0,
            addressed: false,
        }
    }

    fn documented(&self, register: u8) -> bool {
        self.spec
            .registers
            .iter()
            .any(|(known, _)| *known == register)
            || self.spec.reserved.contains(&register)
    }
}

impl I2cDevice for RegisterMapModel {
    fn model(&self) -> &'static str {
        self.spec.model
    }

    fn write(&mut self, context: &mut DeviceContext<'_>, bytes: &[u8]) -> Result<(), DataNack> {
        let Some((&register, data)) = bytes.split_first() else {
            return Ok(());
        };
        if !self.documented(register) {
            context.violation(
                self.spec.map_rule,
                format!("register {register:#04x} is not in the register map"),
            );
        }
        self.pointer = register;
        self.addressed = true;
        match data {
            [] => {}
            [value] => self.registers[usize::from(register)] = *value,
            _ => context.violation(
                self.spec.write_rule,
                format!(
                    "write to {register:#04x} carried {} data bytes; the write format has one",
                    data.len()
                ),
            ),
        }
        Ok(())
    }

    fn read(&mut self, context: &mut DeviceContext<'_>, buffer: &mut [u8]) {
        if !std::mem::take(&mut self.addressed) {
            context.violation(
                self.spec.read_rule,
                "read without first writing the register address",
            );
        }
        if buffer.len() > 1 {
            context.violation(
                self.spec.read_rule,
                format!(
                    "read of {} bytes; the read format returns one",
                    buffer.len()
                ),
            );
        }
        buffer.fill(self.registers[usize::from(self.pointer)]);
    }

    fn stop(&mut self, _context: &mut DeviceContext<'_>) {
        self.addressed = false;
    }

    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
        let source = crate::device::register_range(&self.registers, offset, buffer.len())?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn poke(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RegisterRangeError> {
        let length = bytes.len();
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= self.registers.len())
            .ok_or(RegisterRangeError { offset, length })?;
        self.registers[offset..end].copy_from_slice(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::i2c::I2c as _;

    use crate::{models::everest, Clock, VirtualHardware};

    fn setup() -> (VirtualHardware, crate::VirtualI2cBus) {
        let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
        hardware
            .attach("I2C0", 0x18, Box::new(everest::es8311()))
            .expect("attach");
        (hardware.clone(), hardware.i2c_bus("I2C0").expect("bus"))
    }

    fn rules(hardware: &VirtualHardware) -> Vec<String> {
        hardware
            .violations()
            .into_iter()
            .map(|violation| violation.violation.rule)
            .collect()
    }

    #[test]
    fn defaults_follow_the_register_definitions() {
        let (hardware, mut bus) = setup();
        let mut id = [0];
        bus.write_read(0x18, &[0xfd], &mut id).expect("chip id");
        assert_eq!(id, [0x83]);
        bus.write_read(0x18, &[0x00], &mut id)
            .expect("reset register");
        assert_eq!(id, [0x1f]);
        bus.write(0x18, &[0x32, 0xbf]).expect("volume");
        assert_eq!(
            hardware
                .read_registers("I2C0", 0x18, 0x32, 1)
                .expect("peek"),
            [0xbf]
        );
        assert!(rules(&hardware).is_empty(), "{:?}", hardware.violations());
    }

    #[test]
    fn format_and_map_rules_are_reported() {
        let (hardware, mut bus) = setup();
        bus.write(0x18, &[0x50, 0]).expect("undocumented register");
        bus.write(0x18, &[0x01, 1, 2]).expect("two data bytes");
        let mut value = [0];
        bus.read(0x18, &mut value).expect("read without address");
        let mut two = [0; 2];
        bus.write_read(0x18, &[0x01], &mut two)
            .expect("two-byte read");
        assert_eq!(
            rules(&hardware),
            [
                everest::ES8311.map_rule,
                everest::ES8311.write_rule,
                everest::ES8311.read_rule,
                everest::ES8311.read_rule,
            ]
        );
    }
}

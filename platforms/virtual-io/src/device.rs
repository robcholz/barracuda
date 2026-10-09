//! Behaviour contract for virtual devices attached to a virtual I2C bus.
//!
//! A device model owns its register behaviour and may check the order and
//! timing of the transactions it receives against its datasheet. Models know
//! nothing about the control server or the System: the same model serves a
//! Rust driver test over a standalone [`crate::VirtualHardware`] and an E2E run
//! through the host Platform.

use std::time::Duration;

use serde::Serialize;

/// One datasheet rule that the observed transactions broke.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RuleViolation {
    /// Stable rule identifier, normally citing the datasheet section.
    pub rule: String,
    /// Human-readable description of what was observed.
    pub message: String,
}

/// Time and violation sink handed to a device model for one bus phase.
#[derive(Debug)]
pub struct DeviceContext<'a> {
    now: Duration,
    violations: &'a mut Vec<RuleViolation>,
    notes: Vec<String>,
}

impl<'a> DeviceContext<'a> {
    pub(crate) fn new(now: Duration, violations: &'a mut Vec<RuleViolation>) -> Self {
        Self {
            now,
            violations,
            notes: Vec::new(),
        }
    }

    pub(crate) fn into_notes(self) -> Vec<String> {
        self.notes
    }

    /// Time of this bus phase on the hardware model's monotonic clock.
    #[must_use]
    pub const fn now(&self) -> Duration {
        self.now
    }

    /// Records an observable device-side change, such as an output pin
    /// level, in the hardware event log after the current transaction.
    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// Reports that the observed traffic breaks a datasheet rule.
    pub fn violation(&mut self, rule: impl Into<String>, message: impl Into<String>) {
        self.violations.push(RuleViolation {
            rule: rule.into(),
            message: message.into(),
        });
    }
}

/// A device refused a data byte (NACK after the address was acknowledged).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataNack;

/// Backdoor register access outside the model's register file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("register range {offset}+{length} is outside the device register file")]
pub struct RegisterRangeError {
    /// First byte offset requested.
    pub offset: usize,
    /// Number of bytes requested.
    pub length: usize,
}

/// Behaviour of one device answering a seven-bit I2C address.
///
/// A transaction addressed to the device arrives as phases: each `write`
/// carries the bytes sent between one START (or repeated START) and the next
/// condition, adjacent `embedded-hal` write operations already merged; each
/// `read` fills the bytes clocked out in one read phase. `stop` ends the
/// transaction.
pub trait I2cDevice: Send {
    /// Short model name reported by the control interface.
    fn model(&self) -> &'static str;

    /// Called once when the device is attached (its power-on time).
    fn attached(&mut self, _context: &mut DeviceContext<'_>) {}

    /// Receives one write phase.
    ///
    /// # Errors
    ///
    /// Returns [`DataNack`] when the device does not acknowledge the data.
    fn write(&mut self, context: &mut DeviceContext<'_>, bytes: &[u8]) -> Result<(), DataNack>;

    /// Produces the bytes of one read phase.
    fn read(&mut self, context: &mut DeviceContext<'_>, buffer: &mut [u8]);

    /// Called after the last phase of a transaction (STOP).
    fn stop(&mut self, _context: &mut DeviceContext<'_>) {}

    /// Copies model-defined register bytes without bus side effects.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterRangeError`] when the range is outside the model.
    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError>;

    /// Stores model-defined register bytes without bus side effects.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterRangeError`] when the range is outside the model.
    fn poke(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RegisterRangeError>;
}

/// Size of the generic register file.
pub const REGISTER_FILE_BYTES: usize = 256;

/// Generic register device with an 8-bit auto-incrementing pointer.
///
/// A write phase sets the pointer from its first byte and stores the rest
/// from there; a read phase returns bytes from the pointer. Both increment the
/// pointer after every byte and wrap at 256, like most sensors and a 24C02
/// EEPROM. It has no ordering or timing constraints.
#[derive(Clone, Debug)]
pub struct RegisterDevice {
    registers: [u8; REGISTER_FILE_BYTES],
    pointer: u8,
}

impl RegisterDevice {
    /// Creates a device whose registers all read zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            registers: [0; REGISTER_FILE_BYTES],
            pointer: 0,
        }
    }

    /// Returns the current register pointer.
    #[must_use]
    pub const fn pointer(&self) -> u8 {
        self.pointer
    }
}

impl Default for RegisterDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for RegisterDevice {
    fn model(&self) -> &'static str {
        "registers"
    }

    fn write(&mut self, _context: &mut DeviceContext<'_>, bytes: &[u8]) -> Result<(), DataNack> {
        let Some((&pointer, data)) = bytes.split_first() else {
            return Ok(());
        };
        self.pointer = pointer;
        for &byte in data {
            self.registers[usize::from(self.pointer)] = byte;
            self.pointer = self.pointer.wrapping_add(1);
        }
        Ok(())
    }

    fn read(&mut self, _context: &mut DeviceContext<'_>, buffer: &mut [u8]) {
        for byte in buffer {
            *byte = self.registers[usize::from(self.pointer)];
            self.pointer = self.pointer.wrapping_add(1);
        }
    }

    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
        let source = register_range(&self.registers, offset, buffer.len())?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn poke(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RegisterRangeError> {
        let length = bytes.len();
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= REGISTER_FILE_BYTES)
            .ok_or(RegisterRangeError { offset, length })?;
        self.registers[offset..end].copy_from_slice(bytes);
        Ok(())
    }
}

/// Returns `registers[offset..offset + length]` or a range error.
///
/// # Errors
///
/// Returns [`RegisterRangeError`] when the range does not fit.
pub fn register_range(
    registers: &[u8],
    offset: usize,
    length: usize,
) -> Result<&[u8], RegisterRangeError> {
    offset
        .checked_add(length)
        .and_then(|end| registers.get(offset..end))
        .ok_or(RegisterRangeError { offset, length })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(violations: &mut Vec<RuleViolation>) -> DeviceContext<'_> {
        DeviceContext::new(Duration::ZERO, violations)
    }

    #[test]
    fn a_write_sets_the_pointer_and_stores_with_auto_increment() {
        let mut violations = Vec::new();
        let mut device = RegisterDevice::new();
        assert_eq!(
            device.write(&mut context(&mut violations), &[0x10, 1, 2, 3]),
            Ok(())
        );
        assert_eq!(device.pointer(), 0x13);
        let mut stored = [0; 3];
        assert_eq!(device.peek(0x10, &mut stored), Ok(()));
        assert_eq!(stored, [1, 2, 3]);
    }

    #[test]
    fn a_read_continues_from_the_pointer_and_wraps() {
        let mut violations = Vec::new();
        let mut device = RegisterDevice::new();
        assert_eq!(device.poke(0xfe, &[0xaa, 0xbb]), Ok(()));
        assert_eq!(device.poke(0, &[0xcc]), Ok(()));
        assert_eq!(device.write(&mut context(&mut violations), &[0xfe]), Ok(()));
        let mut read = [0; 3];
        device.read(&mut context(&mut violations), &mut read);
        assert_eq!(read, [0xaa, 0xbb, 0xcc]);
        assert_eq!(device.pointer(), 1);
        assert!(violations.is_empty());
    }

    #[test]
    fn an_empty_write_is_a_probe_that_keeps_the_pointer() {
        let mut violations = Vec::new();
        let mut device = RegisterDevice::new();
        assert_eq!(device.write(&mut context(&mut violations), &[7]), Ok(()));
        assert_eq!(device.write(&mut context(&mut violations), &[]), Ok(()));
        assert_eq!(device.pointer(), 7);
    }

    #[test]
    fn backdoor_access_rejects_ranges_outside_the_register_file() {
        let mut device = RegisterDevice::new();
        assert!(device.poke(255, &[1, 2]).is_err());
        let mut buffer = [0; 2];
        assert!(device.peek(255, &mut buffer).is_err());
        assert_eq!(device.peek(254, &mut buffer), Ok(()));
    }
}

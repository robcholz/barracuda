//! Texas Instruments BQ27220 single-cell fuel gauge.
//!
//! Sources:
//! - TI datasheet SLUSCB7A, "bq27220 Single-Cell CEDV Fuel Gauge", March
//!   2016, revised April 2016 (sections 6.8, 6.11, 7.3.1);
//! - TI technical reference manual SLUUBD4A, "BQ27220 Technical Reference
//!   Manual", April 2016, revised November 2022 (Table 2-1).
//!
//! Modelled: the standard commands of SLUUBD4A Table 2-1 as little-endian
//! 16-bit values, the incremental address pointer (SLUSCB7A 7.3.1.1), the
//! NACK of a write to a read-only command (7.3.1.1 Figure 7), the bus-free
//! wait between packets (6.11 t(BUF), 7.3.1.3), the standard-command rate
//! limit (7.3.1.3), and the power-up communication delay (6.8 tPUCD). Not
//! modelled: Control() subcommands and MACData(), data flash, SEALED and
//! UNSEALED modes (SEALED access is assumed), gauging, and clock stretching.
//!
//! Backdoor layout: byte offset equals the command code (00h–7Fh); poke the
//! little-endian telemetry values a test expects the gauge to report.

use std::{collections::VecDeque, time::Duration};

use crate::device::{DataNack, DeviceContext, I2cDevice, RegisterRangeError};

const COMMAND_BYTES: usize = 0x80;

/// 6.11 and 7.3.1.3: t(BUF) ≥ 66 µs between all packets to the gauge.
const BUS_FREE: Duration = Duration::from_micros(66);
/// 6.8: tPUCD, power-up communication delay, 250 ms (typical; the
/// datasheet gives no minimum or maximum).
const POWER_UP_COMMUNICATION_DELAY: Duration = Duration::from_millis(250);
/// 7.3.1.3: no standard command more than two times per second.
const COMMANDS_PER_SECOND: usize = 2;

/// SLUUBD4A Table 2-1 command codes, by first byte, with SEALED write access.
const fn command(code: u8) -> Option<bool> {
    match code {
        // Control(), AtRate(), Temperature(): RW.
        0x00 | 0x01 | 0x02 | 0x03 | 0x06 | 0x07 => Some(true),
        // Read-only standard commands.
        0x04 | 0x05 | 0x08..=0x0d | 0x10..=0x25 | 0x28..=0x33 | 0x3a..=0x3d => Some(false),
        // BTPDischargeSet(), BTPChargeSet(): access not listed for SEALED.
        0x34..=0x37 => Some(true),
        // MACData(), MACDataSum(), MACDataLen(), AnalogCount(), Raw*().
        0x40..=0x61 | 0x79..=0x7f => Some(true),
        _ => None,
    }
}

/// Rule identifiers reported by this model.
pub mod rules {
    /// SLUSCB7A 6.11 and 7.3.1.3: at least 66 µs bus-free time between packets.
    pub const BUS_FREE: &str = "BQ27220 SLUSCB7A 7.3.1.3 t(BUF) 66 us between packets";
    /// SLUSCB7A 7.3.1.3: a standard command at most twice per second.
    pub const COMMAND_RATE: &str = "BQ27220 SLUSCB7A 7.3.1.3 standard command rate";
    /// SLUSCB7A 6.8: no communication before tPUCD (250 ms typical).
    pub const POWER_UP: &str = "BQ27220 SLUSCB7A 6.8 tPUCD power-up communication delay";
    /// SLUUBD4A Table 2-1: the command code is a standard command.
    pub const COMMAND_MAP: &str = "BQ27220 SLUUBD4A Table 2-1 command code";
}

/// Behavioural model of one BQ27220.
#[derive(Clone, Debug)]
pub struct Bq27220Model {
    commands: [u8; COMMAND_BYTES],
    pointer: u8,
    powered_at: Duration,
    last_packet_end: Option<Duration>,
    issued: Vec<VecDeque<Duration>>,
    checked_bus_free: bool,
}

impl Bq27220Model {
    /// Creates a gauge whose commands all read zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            commands: [0; COMMAND_BYTES],
            pointer: 0,
            powered_at: Duration::ZERO,
            last_packet_end: None,
            issued: vec![VecDeque::new(); COMMAND_BYTES],
            checked_bus_free: false,
        }
    }

    /// Sets one 16-bit standard command value (little-endian on the bus).
    #[must_use]
    pub fn with_word(mut self, code: u8, value: u16) -> Self {
        let start = usize::from(code);
        if start + 1 < COMMAND_BYTES {
            self.commands[start..start + 2].copy_from_slice(&value.to_le_bytes());
        }
        self
    }

    fn begin_packet(&mut self, context: &mut DeviceContext<'_>) {
        if std::mem::replace(&mut self.checked_bus_free, true) {
            return;
        }
        let now = context.now();
        if now < self.powered_at + POWER_UP_COMMUNICATION_DELAY {
            context.violation(
                rules::POWER_UP,
                format!(
                    "packet {} ms after power-up; tPUCD is {} ms",
                    (now - self.powered_at).as_millis(),
                    POWER_UP_COMMUNICATION_DELAY.as_millis()
                ),
            );
        }
        if let Some(end) = self.last_packet_end.filter(|end| now < *end + BUS_FREE) {
            context.violation(
                rules::BUS_FREE,
                format!(
                    "packet {} µs after the previous one; t(BUF) is {} µs",
                    (now - end).as_micros(),
                    BUS_FREE.as_micros()
                ),
            );
        }
    }

    fn issue(&mut self, context: &mut DeviceContext<'_>, code: u8) {
        let now = context.now();
        let Some(history) = self.issued.get_mut(usize::from(code & !1)) else {
            return;
        };
        while history
            .front()
            .is_some_and(|time| now.saturating_sub(*time) >= Duration::from_secs(1))
        {
            let _expired = history.pop_front();
        }
        history.push_back(now);
        if history.len() > COMMANDS_PER_SECOND {
            context.violation(
                rules::COMMAND_RATE,
                format!(
                    "standard command {:#04x} issued {} times within one second",
                    code & !1,
                    history.len()
                ),
            );
        }
    }
}

impl Default for Bq27220Model {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Bq27220Model {
    fn model(&self) -> &'static str {
        "bq27220"
    }

    fn attached(&mut self, context: &mut DeviceContext<'_>) {
        self.powered_at = context.now();
    }

    fn write(&mut self, context: &mut DeviceContext<'_>, bytes: &[u8]) -> Result<(), DataNack> {
        self.begin_packet(context);
        let Some((&code, data)) = bytes.split_first() else {
            return Ok(());
        };
        let Some(writable) = command(code) else {
            context.violation(
                rules::COMMAND_MAP,
                format!("command code {code:#04x} is not a standard command"),
            );
            return Err(DataNack);
        };
        self.pointer = code;
        if code < 0x40 {
            self.issue(context, code);
        }
        if !data.is_empty() && !writable {
            // SLUSCB7A 7.3.1.1 Figure 7: NACK after the data of a write to a
            // read-only address.
            return Err(DataNack);
        }
        for &byte in data {
            if let Some(slot) = self.commands.get_mut(usize::from(self.pointer)) {
                *slot = byte;
            }
            self.pointer = self.pointer.wrapping_add(1);
        }
        Ok(())
    }

    fn read(&mut self, context: &mut DeviceContext<'_>, buffer: &mut [u8]) {
        self.begin_packet(context);
        // 7.3.1.1: the address pointer increments with every acknowledged byte.
        for byte in buffer {
            *byte = self
                .commands
                .get(usize::from(self.pointer))
                .copied()
                .unwrap_or(0);
            self.pointer = self.pointer.wrapping_add(1);
        }
    }

    fn stop(&mut self, context: &mut DeviceContext<'_>) {
        self.checked_bus_free = false;
        self.last_packet_end = Some(context.now());
    }

    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
        let source = crate::device::register_range(&self.commands, offset, buffer.len())?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn poke(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RegisterRangeError> {
        let length = bytes.len();
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= COMMAND_BYTES)
            .ok_or(RegisterRangeError { offset, length })?;
        self.commands[offset..end].copy_from_slice(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::i2c::{I2c as _, NoAcknowledgeSource};

    use super::*;
    use crate::{Clock, VirtualHardware, VirtualI2cError};

    const ADDRESS: u8 = 0x55;

    fn setup(model: Bq27220Model) -> (VirtualHardware, crate::VirtualI2cBus) {
        let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
        hardware
            .attach("I2C0", u64::from(ADDRESS), Box::new(model))
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
    fn spaced_reads_return_little_endian_words_without_violations() {
        let (hardware, mut bus) = setup(Bq27220Model::new().with_word(0x08, 3_925));
        hardware.clock().advance(Duration::from_millis(250));
        let mut voltage = [0; 2];
        bus.write_read(ADDRESS, &[0x08], &mut voltage)
            .expect("voltage");
        assert_eq!(u16::from_le_bytes(voltage), 3_925);
        hardware.clock().advance(Duration::from_micros(66));
        let mut both = [0; 4];
        bus.write_read(ADDRESS, &[0x06], &mut both)
            .expect("incremental");
        assert_eq!(both, [0, 0, 0x55, 0x0f]);
        assert!(rules(&hardware).is_empty(), "{:?}", hardware.violations());
    }

    #[test]
    fn timing_rules_cover_power_up_bus_free_and_rate() {
        let (hardware, mut bus) = setup(Bq27220Model::new());
        let mut word = [0; 2];
        bus.write_read(ADDRESS, &[0x08], &mut word).expect("early");
        hardware.clock().advance(Duration::from_millis(250));
        bus.write_read(ADDRESS, &[0x08], &mut word).expect("second");
        hardware.clock().advance(Duration::from_micros(65));
        bus.write_read(ADDRESS, &[0x08], &mut word).expect("third");
        assert_eq!(
            rules(&hardware),
            [rules::POWER_UP, rules::BUS_FREE, rules::COMMAND_RATE]
        );
    }

    #[test]
    fn a_write_to_a_read_only_command_is_not_acknowledged() {
        let (hardware, mut bus) = setup(Bq27220Model::new());
        hardware.clock().advance(Duration::from_millis(250));
        assert_eq!(
            bus.write(ADDRESS, &[0x08, 1, 2]),
            Err(VirtualI2cError::NoAcknowledge(NoAcknowledgeSource::Data))
        );
        hardware.clock().advance(Duration::from_millis(1));
        assert_eq!(bus.write(ADDRESS, &[0x02, 0x10, 0x00]), Ok(()));
        assert_eq!(
            hardware.read_registers("I2C0", 0x55, 2, 2).expect("peek"),
            [0x10, 0]
        );
    }
}

//! Hynitron CST816T capacitive touch controller (CST816T datasheet V1.1,
//! register map in its I2C interface section).
//!
//! The host reads a six-byte report from register 01h: gesture, finger
//! count, then X and Y as 12-bit values whose high nibbles share bytes with
//! the event flags. Register A7h holds the chip ID (B4h for the CST816T).
//!
//! Backdoor layout for `read_registers`/`write_registers`: byte 0 is 1 while a
//! finger touches and 0 otherwise; bytes 1..=2 and 3..=4 are X and Y,
//! big-endian. Writing them is how a client touches the panel. Like the chip,
//! which holds a contact until the host reads it, a touch lifted before any
//! report was read still appears in the next report.

use crate::device::{DataNack, DeviceContext, I2cDevice, RegisterRangeError};

/// First report register: gesture ID.
const GESTURE_ID: u8 = 0x01;
/// Chip ID register.
const CHIP_ID: u8 = 0xa7;
/// CST816T chip ID.
const CST816T: u8 = 0xb4;
/// Event flags in the X high byte: contact (10b) or lift-off (01b).
const EVENT_CONTACT: u8 = 0x80;
const EVENT_LIFT: u8 = 0x40;
/// Backdoor bytes: touched, X high, X low, Y high, Y low.
const BACKDOOR_BYTES: usize = 5;

/// A CST816T touch controller.
#[derive(Debug, Default)]
pub struct Cst816tModel {
    pointer: u8,
    touched: bool,
    x: u16,
    y: u16,
    /// The current contact has not appeared in a report yet.
    unreported: bool,
    /// The finger lifted before its contact was reported.
    lift_pending: bool,
}

impl Cst816tModel {
    /// A controller with no finger on the panel.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pointer: 0,
            touched: false,
            x: 0,
            y: 0,
            unreported: false,
            lift_pending: false,
        }
    }

    /// Puts a finger at (`x`, `y`), or lifts it.
    pub const fn touch(&mut self, point: Option<(u16, u16)>) {
        match point {
            Some((x, y)) => {
                if !self.touched {
                    self.unreported = true;
                }
                self.touched = true;
                self.lift_pending = false;
                self.x = x & 0x0fff;
                self.y = y & 0x0fff;
            }
            None if self.unreported => self.lift_pending = true,
            None => self.touched = false,
        }
    }

    fn register(&self, register: u8) -> u8 {
        let event = if self.touched {
            EVENT_CONTACT
        } else {
            EVENT_LIFT
        };
        let [x_high, x_low] = self.x.to_be_bytes();
        let [y_high, y_low] = self.y.to_be_bytes();
        match register {
            GESTURE_ID => 0,
            0x02 => u8::from(self.touched),
            0x03 => event | x_high,
            0x04 => x_low,
            0x05 => y_high,
            0x06 => y_low,
            CHIP_ID => CST816T,
            _ => 0,
        }
    }

    fn backdoor(&self) -> [u8; BACKDOOR_BYTES] {
        let [x_high, x_low] = self.x.to_be_bytes();
        let [y_high, y_low] = self.y.to_be_bytes();
        [
            u8::from(self.touched && !self.lift_pending),
            x_high,
            x_low,
            y_high,
            y_low,
        ]
    }
}

impl I2cDevice for Cst816tModel {
    fn model(&self) -> &'static str {
        "cst816t"
    }

    fn write(&mut self, _context: &mut DeviceContext<'_>, bytes: &[u8]) -> Result<(), DataNack> {
        // Configuration writes after the pointer, such as disabling auto
        // sleep, change nothing the host can observe here.
        if let Some(pointer) = bytes.first() {
            self.pointer = *pointer;
        }
        Ok(())
    }

    fn read(&mut self, _context: &mut DeviceContext<'_>, buffer: &mut [u8]) {
        let mut reported = false;
        for byte in buffer {
            reported |= self.pointer == 0x02;
            *byte = self.register(self.pointer);
            self.pointer = self.pointer.wrapping_add(1);
        }
        if reported && self.touched {
            self.unreported = false;
            if std::mem::take(&mut self.lift_pending) {
                self.touched = false;
            }
        }
    }

    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
        let backdoor = self.backdoor();
        let source = crate::device::register_range(&backdoor, offset, buffer.len())?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn poke(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RegisterRangeError> {
        let mut backdoor = self.backdoor();
        crate::device::register_range(&backdoor, offset, bytes.len())?;
        backdoor[offset..offset + bytes.len()].copy_from_slice(bytes);
        let [touched, x_high, x_low, y_high, y_low] = backdoor;
        self.touch((touched != 0).then_some((
            u16::from_be_bytes([x_high, x_low]),
            u16::from_be_bytes([y_high, y_low]),
        )));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::i2c::I2c as _;

    use super::*;
    use crate::{hardware::VirtualHardware, Clock};

    const ADDRESS: u8 = 0x15;

    #[test]
    fn a_finger_set_through_the_backdoor_appears_in_the_report() {
        let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
        hardware
            .attach("I2C0", u64::from(ADDRESS), Box::new(Cst816tModel::new()))
            .expect("attach");
        let mut bus = hardware.i2c_bus("I2C0").expect("bus");

        let mut id = [0];
        bus.write_read(ADDRESS, &[CHIP_ID], &mut id).expect("id");
        assert_eq!(id, [CST816T]);

        let mut report = [0; 6];
        bus.write_read(ADDRESS, &[GESTURE_ID], &mut report)
            .expect("idle");
        assert_eq!(report[1], 0);

        hardware
            .write_registers("I2C0", u64::from(ADDRESS), 0, &[1, 0x01, 0x23, 0x00, 0x45])
            .expect("touch");
        bus.write_read(ADDRESS, &[GESTURE_ID], &mut report)
            .expect("touched");
        assert_eq!(report, [0, 1, 0x81, 0x23, 0x00, 0x45]);

        hardware
            .write_registers("I2C0", u64::from(ADDRESS), 0, &[0])
            .expect("lift");
        bus.write_read(ADDRESS, &[GESTURE_ID], &mut report)
            .expect("lifted");
        assert_eq!(report[1], 0);

        // A tap lifted before the host polled is still reported once.
        hardware
            .write_registers("I2C0", u64::from(ADDRESS), 0, &[1, 0, 9, 0, 8])
            .expect("tap");
        hardware
            .write_registers("I2C0", u64::from(ADDRESS), 0, &[0])
            .expect("release");
        bus.write_read(ADDRESS, &[GESTURE_ID], &mut report)
            .expect("tap report");
        assert_eq!(report, [0, 1, 0x80, 9, 0, 8]);
        bus.write_read(ADDRESS, &[GESTURE_ID], &mut report)
            .expect("after");
        assert_eq!(report[1], 0);
    }
}

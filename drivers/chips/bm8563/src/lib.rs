//! Register-level driver for the BM8563 real-time clock and the NXP PCF8563
//! it is register-compatible with.
//!
//! Registers follow the NXP PCF8563 datasheet (Rev. 11, section 8): control
//! and status 1 and 2 at `00h`/`01h`, the clock and calendar from `02h`
//! (seconds, whose bit 7 is the voltage-low flag VL) to `08h` (years), and
//! the timer control at `0Eh`. The month register's bit 7 is the century
//! flag; as in M5Stack's M5Unified `PCF8563_Class`, a set flag means the
//! 1900s, so the chip holds 1900 through 2099.
//!
//! The upstream `pcf8563` crate is not used: it accepts any register value
//! without checking that it is BCD, ignores the century flag, and its
//! `set_datetime` bounds the month instead of the day by 31.

#![no_std]

use embedded_hal::i2c::I2c;

/// The fixed seven-bit I2C address of the BM8563 and the PCF8563.
pub const ADDRESS: u8 = 0x51;

const CONTROL_STATUS_1: u8 = 0x00;
const SECONDS: u8 = 0x02;
const TIMER_CONTROL: u8 = 0x0e;
/// VL: the oscillator stopped or the supply fell below its limit, so the
/// clock is no longer guaranteed.
const VOLTAGE_LOW: u8 = 1 << 7;
/// C: the century flag in the months register.
const CENTURY: u8 = 1 << 7;
/// Timer disabled, at its lowest-power 1/60 Hz source (datasheet table 33);
/// M5Unified `PCF8563_Class::begin` writes the same value.
const TIMER_DISABLED: u8 = 0x03;

/// Chip-native clock and calendar fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockData {
    /// Full year, 1900 through 2099.
    pub year: u16,
    /// One-based month.
    pub month: u8,
    /// One-based day of month.
    pub day: u8,
    /// Weekday, 0 through 6, as the application counts it.
    pub weekday: u8,
    /// Hour in 24-hour format.
    pub hour: u8,
    /// Minute.
    pub minute: u8,
    /// Second.
    pub second: u8,
}

/// BM8563 transport or clock-data failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// The clock registers do not hold a valid BCD date and time.
    InvalidClockData,
    /// The requested date and time cannot be represented by the chip.
    InvalidDateTime,
}

/// A BM8563 or PCF8563 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bm8563 {
    address: u8,
}

impl Bm8563 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Starts the clock in normal mode and disables the countdown timer.
    ///
    /// The first write is repeated, as M5Unified does, because the TimerCam's
    /// clock may not acknowledge the first transfer after power-up.
    ///
    /// # Errors
    ///
    /// Returns the bus error of the repeated or the timer write.
    pub fn start<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), I2C::Error> {
        // Only the repeated write's result counts.
        let _ = self.write_register(i2c, CONTROL_STATUS_1, 0x00);
        self.write_register(i2c, CONTROL_STATUS_1, 0x00)?;
        self.write_register(i2c, TIMER_CONTROL, TIMER_DISABLED)
    }

    /// Reports the voltage-low flag: whether the retained time may be invalid.
    ///
    /// # Errors
    ///
    /// Returns the bus error.
    pub fn lost_power<I2C: I2c>(&self, i2c: &mut I2C) -> Result<bool, I2C::Error> {
        let mut seconds = [0];
        i2c.write_read(self.address, &[SECONDS], &mut seconds)?;
        Ok(seconds[0] & VOLTAGE_LOW != 0)
    }

    /// Reads the seven clock and calendar registers in one transfer.
    ///
    /// # Errors
    ///
    /// Returns a bus error, or [`Error::InvalidClockData`] when a field is
    /// not BCD or out of range.
    pub fn read_clock<I2C: I2c>(&self, i2c: &mut I2C) -> Result<ClockData, Error<I2C::Error>> {
        let mut data = [0; 7];
        i2c.write_read(self.address, &[SECONDS], &mut data)
            .map_err(Error::Bus)?;
        decode(data).ok_or(Error::InvalidClockData)
    }

    /// Writes the seven clock and calendar registers in one transfer. The
    /// seconds byte clears the voltage-low flag.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDateTime`] for a value the chip cannot hold,
    /// or a bus error.
    pub fn write_clock<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        clock: ClockData,
    ) -> Result<(), Error<I2C::Error>> {
        let data = encode(clock).ok_or(Error::InvalidDateTime)?;
        let mut write = [0; 8];
        write[0] = SECONDS;
        write[1..].copy_from_slice(&data);
        i2c.write(self.address, &write).map_err(Error::Bus)
    }

    fn write_register<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        register: u8,
        value: u8,
    ) -> Result<(), I2C::Error> {
        i2c.write(self.address, &[register, value])
    }
}

fn decode(data: [u8; 7]) -> Option<ClockData> {
    let century_1900 = data[5] & CENTURY != 0;
    let years = u16::from(from_bcd(data[6])?);
    let clock = ClockData {
        second: from_bcd(data[0] & 0x7f)?,
        minute: from_bcd(data[1] & 0x7f)?,
        hour: from_bcd(data[2] & 0x3f)?,
        day: from_bcd(data[3] & 0x3f)?,
        weekday: data[4] & 0x07,
        month: from_bcd(data[5] & 0x1f)?,
        year: if century_1900 { 1900_u16 } else { 2000 }.checked_add(years)?,
    };
    valid(clock).then_some(clock)
}

fn encode(clock: ClockData) -> Option<[u8; 7]> {
    if !valid(clock) {
        return None;
    }
    let (century, years) = if clock.year < 2000 {
        (CENTURY, clock.year.checked_sub(1900)?)
    } else {
        (0, clock.year.checked_sub(2000)?)
    };
    Some([
        to_bcd(clock.second),
        to_bcd(clock.minute),
        to_bcd(clock.hour),
        to_bcd(clock.day),
        clock.weekday,
        to_bcd(clock.month) | century,
        to_bcd(u8::try_from(years).ok()?),
    ])
}

fn valid(clock: ClockData) -> bool {
    (1900..=2099).contains(&clock.year)
        && (1..=12).contains(&clock.month)
        && (1..=31).contains(&clock.day)
        && clock.weekday <= 6
        && clock.hour <= 23
        && clock.minute <= 59
        && clock.second <= 59
}

const fn from_bcd(value: u8) -> Option<u8> {
    let high = value >> 4;
    let low = value & 0x0f;
    if high > 9 || low > 9 {
        None
    } else {
        Some(high.saturating_mul(10).saturating_add(low))
    }
}

const fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::{vec, vec::Vec};

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{ADDRESS, Bm8563, ClockData, Error};

    /// A register file that auto-increments like the chip.
    struct Clock {
        registers: [u8; 0x10],
        writes: Vec<Vec<u8>>,
    }

    impl Clock {
        fn new() -> Self {
            Self {
                registers: [0; 0x10],
                writes: Vec::new(),
            }
        }
    }

    impl ErrorType for Clock {
        type Error = Infallible;
    }

    impl I2c for Clock {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, ADDRESS);
            let mut pointer = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        pointer = usize::from(bytes[0]);
                        for (offset, value) in bytes[1..].iter().enumerate() {
                            self.registers[pointer + offset] = *value;
                        }
                        if bytes.len() > 1 {
                            self.writes.push(bytes.to_vec());
                        }
                    }
                    Operation::Read(bytes) => {
                        for (offset, value) in bytes.iter_mut().enumerate() {
                            *value = self.registers[pointer + offset];
                        }
                    }
                }
            }
            Ok(())
        }
    }

    fn clock() -> Bm8563 {
        Bm8563::new(ADDRESS).expect("valid address")
    }

    #[test]
    fn start_clears_the_stop_bit_twice_and_disables_the_timer() {
        let mut bus = Clock::new();
        bus.registers[0x00] = 0x20;
        clock().start(&mut bus).expect("start");
        assert_eq!(
            bus.writes,
            [vec![0x00, 0x00], vec![0x00, 0x00], vec![0x0e, 0x03]]
        );
    }

    #[test]
    fn decodes_bcd_fields_and_masks_status_bits() {
        let mut bus = Clock::new();
        // 2026-10-21 (Wednesday) 13:45:07 with VL set.
        bus.registers[0x02..0x09].copy_from_slice(&[0x87, 0x45, 0x13, 0x21, 0x03, 0x10, 0x26]);
        assert!(clock().lost_power(&mut bus).expect("flag"));
        assert_eq!(
            clock().read_clock(&mut bus).expect("clock"),
            ClockData {
                year: 2026,
                month: 10,
                day: 21,
                weekday: 3,
                hour: 13,
                minute: 45,
                second: 7,
            }
        );
    }

    #[test]
    fn the_century_flag_selects_the_1900s() {
        let mut bus = Clock::new();
        let date = ClockData {
            year: 1999,
            month: 12,
            day: 31,
            weekday: 5,
            hour: 23,
            minute: 59,
            second: 58,
        };
        clock().write_clock(&mut bus, date).expect("write");
        assert_eq!(
            bus.writes,
            [vec![0x02, 0x58, 0x59, 0x23, 0x31, 0x05, 0x92, 0x99]]
        );
        assert_eq!(clock().read_clock(&mut bus).expect("clock"), date);
        assert!(!clock().lost_power(&mut bus).expect("flag"));
    }

    #[test]
    fn rejects_corrupt_registers_and_unrepresentable_dates() {
        let mut bus = Clock::new();
        bus.registers[0x02..0x09].copy_from_slice(&[0x5a, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00]);
        assert!(matches!(
            clock().read_clock(&mut bus),
            Err(Error::InvalidClockData)
        ));
        bus.registers[0x02..0x09].copy_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x00, 0x13, 0x00]);
        assert!(matches!(
            clock().read_clock(&mut bus),
            Err(Error::InvalidClockData)
        ));
        let mut date = ClockData {
            year: 2100,
            month: 1,
            day: 1,
            weekday: 0,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert!(matches!(
            clock().write_clock(&mut bus, date),
            Err(Error::InvalidDateTime)
        ));
        date.year = 2000;
        date.day = 32;
        assert!(matches!(
            clock().write_clock(&mut bus, date),
            Err(Error::InvalidDateTime)
        ));
        assert!(Bm8563::new(0x80).is_none());
    }
}

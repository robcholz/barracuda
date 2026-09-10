//! Register-level RX8130CE real-time-clock driver.

#![no_std]

use embedded_hal::i2c::I2c;

const CLOCK_START: u8 = 0x10;
const FLAG: u8 = 0x1d;
const CONTROL: u8 = 0x1e;
const VOLTAGE_LOW_FLAG: u8 = 1 << 1;
const STOP: u8 = 1 << 6;

/// Chip-native clock and calendar fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockData {
    /// Full year in the chip's 2000–2099 range.
    pub year: u16,
    /// One-based month.
    pub month: u8,
    /// One-based day of month.
    pub day: u8,
    /// Zero-based weekday.
    pub weekday: u8,
    /// Hour in 24-hour format.
    pub hour: u8,
    /// Minute.
    pub minute: u8,
    /// Second.
    pub second: u8,
}

/// RX8130CE transport or clock-data failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// Clock registers did not contain valid BCD calendar data.
    InvalidClockData,
    /// The requested calendar data cannot be represented by the chip.
    InvalidDateTime,
}

/// An RX8130CE at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rx8130ce {
    address: u8,
}

impl Rx8130ce {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Probes the status register.
    pub fn probe<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), I2C::Error> {
        self.read_register(i2c, FLAG).map(|_| ())
    }

    /// Reports the oscillator-stop/supply-loss condition.
    pub fn lost_power<I2C: I2c>(&self, i2c: &mut I2C) -> Result<bool, I2C::Error> {
        self.read_register(i2c, FLAG)
            .map(|value| value & VOLTAGE_LOW_FLAG != 0)
    }

    /// Reads and validates the chip's clock registers.
    pub fn read_clock<I2C: I2c>(&self, i2c: &mut I2C) -> Result<ClockData, Error<I2C::Error>> {
        let mut data = [0; 7];
        i2c.write_read(self.address, &[CLOCK_START], &mut data)
            .map_err(Error::Bus)?;
        let second = from_bcd(data[0] & 0x7f).ok_or(Error::InvalidClockData)?;
        let minute = from_bcd(data[1] & 0x7f).ok_or(Error::InvalidClockData)?;
        let hour = from_bcd(data[2] & 0x3f).ok_or(Error::InvalidClockData)?;
        let weekday_bits = data[3] & 0x7f;
        if weekday_bits.count_ones() != 1 {
            return Err(Error::InvalidClockData);
        }
        let weekday =
            u8::try_from(weekday_bits.trailing_zeros()).map_err(|_| Error::InvalidClockData)?;
        let clock = ClockData {
            year: 2000_u16
                .saturating_add(u16::from(from_bcd(data[6]).ok_or(Error::InvalidClockData)?)),
            month: from_bcd(data[5] & 0x1f).ok_or(Error::InvalidClockData)?,
            day: from_bcd(data[4] & 0x3f).ok_or(Error::InvalidClockData)?,
            weekday,
            hour,
            minute,
            second,
        };
        validate_clock(clock).map_err(|()| Error::InvalidClockData)?;
        Ok(clock)
    }

    /// Writes the clock registers and clears the supply-loss flag.
    pub fn write_clock<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        clock: ClockData,
    ) -> Result<(), Error<I2C::Error>> {
        validate_clock(clock).map_err(|()| Error::InvalidDateTime)?;
        let control = self.read_register(i2c, CONTROL).map_err(Error::Bus)?;
        self.write_register(i2c, CONTROL, control | STOP)
            .map_err(Error::Bus)?;
        let year =
            u8::try_from(clock.year.saturating_sub(2000)).map_err(|_| Error::InvalidDateTime)?;
        let data = [
            CLOCK_START,
            to_bcd(clock.second),
            to_bcd(clock.minute),
            to_bcd(clock.hour),
            1_u8 << clock.weekday,
            to_bcd(clock.day),
            to_bcd(clock.month),
            to_bcd(year),
        ];
        i2c.write(self.address, &data).map_err(Error::Bus)?;
        self.write_register(i2c, CONTROL, control & !STOP)
            .map_err(Error::Bus)?;
        let flag = self.read_register(i2c, FLAG).map_err(Error::Bus)?;
        self.write_register(i2c, FLAG, flag & !VOLTAGE_LOW_FLAG)
            .map_err(Error::Bus)
    }

    fn read_register<I2C: I2c>(&self, i2c: &mut I2C, register: u8) -> Result<u8, I2C::Error> {
        let mut value = [0];
        i2c.write_read(self.address, &[register], &mut value)?;
        Ok(value[0])
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

fn validate_clock(clock: ClockData) -> Result<(), ()> {
    if !(2000..=2099).contains(&clock.year)
        || !(1..=12).contains(&clock.month)
        || !(1..=31).contains(&clock.day)
        || clock.weekday > 6
        || clock.hour > 23
        || clock.minute > 59
        || clock.second > 59
    {
        return Err(());
    }
    Ok(())
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

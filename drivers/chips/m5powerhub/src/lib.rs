//! Register driver for the M5Stack PowerHub's power co-processor.
//!
//! The PowerHub's STM32G031 runs M5Stack firmware that answers at 50h on the
//! ESP32-S3's internal bus (SDA GPIO45, SCL GPIO48). Behind it, on its own
//! bus, sit the RX8130CE real-time clock, six INA226 monitors, the eight
//! WS2812C LEDs, two of the buttons, and the port power switches. No
//! register map is published; every register here is one M5Unified uses:
//!
//! | Register | Use | M5Unified |
//! | --- | --- | --- |
//! | 00h | LED supply enable | `LED_PowerHub_Class::begin` |
//! | 01h–04h | USB, Port A, Port C, and RS485/CAN 5 V enable | `Power_Class::setExtOutput` |
//! | 05h | Power metering enable | `Power_Class::begin` |
//! | 06h | Battery charge enable | `Power_Class::setBatteryCharge` |
//! | 30h | Battery voltage, mV, u16 little-endian | `Power_Class::getBatteryVoltage` |
//! | 32h | Battery current, mA, i16 little-endian, discharge positive | `Power_Class::getBatteryCurrent` |
//! | 34h–47h | Per-port voltage and current, as 30h/32h | `Power_Class::_readExtValue` |
//! | 60h–7Fh | LED colors, four bytes each: blue, green, red, 0 | `LED_PowerHub_Class::display` |
//! | 80h–87h | LED brightness, one byte each | `LED_PowerHub_Class::setBrightness` |
//! | A0h | Buttons; bit 0 low while button B is pressed | `M5Unified::update` |
//! | C0h–C6h | Clock: second, minute, hour, day, month, year − 2000, weekday | `RTC_PowerHub_Class` |
//!
//! The clock fields are binary. The weekday is the RX8130's one-hot weekday
//! bit read as if it were BCD (Sunday 1, Monday 2, … Thursday 10, Friday
//! 20, Saturday 40), which is how M5Unified writes and decodes it.

#![no_std]

use embedded_hal::i2c::I2c;

/// The co-processor's address (M5Unified `powerhub_i2c_addr`).
pub const DEFAULT_ADDRESS: u8 = 0x50;
/// Number of WS2812C LEDs behind the co-processor.
pub const LED_COUNT: usize = 8;

const LED_POWER: u8 = 0x00;
const METER_ENABLE: u8 = 0x05;
const BATTERY_VOLTAGE: u8 = 0x30;
const BATTERY_CURRENT: u8 = 0x32;
const LED_COLORS: u8 = 0x60;
const LED_BRIGHTNESS: u8 = 0x80;
const BUTTONS: u8 = 0xa0;
const CLOCK: u8 = 0xc0;
/// Button B's bit in the button register; the bit reads low while pressed.
const BUTTON_B: u8 = 1 << 0;
/// The weekday codes M5Unified writes, Sunday first.
const WEEKDAY_CODES: [u8; 7] = [1, 2, 4, 8, 10, 20, 40];

/// A power output the co-processor switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerOutput {
    /// The supply of the eight LEDs.
    Leds,
    /// The USB-A port's 5 V.
    Usb,
    /// Port A's 5 V.
    PortA,
    /// Port C's 5 V.
    PortC,
    /// The RS485 and CAN connectors' supply.
    Bus,
}

impl PowerOutput {
    const fn register(self) -> u8 {
        match self {
            Self::Leds => LED_POWER,
            Self::Usb => 0x01,
            Self::PortA => 0x02,
            Self::PortC => 0x03,
            Self::Bus => 0x04,
        }
    }
}

/// Clock fields as the co-processor holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockData {
    /// Full year in the 2000–2099 range.
    pub year: u16,
    /// One-based month.
    pub month: u8,
    /// One-based day of month.
    pub day: u8,
    /// Zero-based weekday, Sunday 0.
    pub weekday: u8,
    /// Hour in 24-hour format.
    pub hour: u8,
    /// Minute.
    pub minute: u8,
    /// Second.
    pub second: u8,
}

/// Transport or clock-data failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// The clock registers do not hold a valid date and time.
    InvalidClockData,
    /// The date or time cannot be stored.
    InvalidDateTime,
}

/// The co-processor at a caller-selected seven-bit address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct M5PowerHub {
    address: u8,
}

impl M5PowerHub {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Switches one power output.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the write fails.
    pub fn set_power<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        output: PowerOutput,
        enable: bool,
    ) -> Result<(), I2C::Error> {
        i2c.write(self.address, &[output.register(), u8::from(enable)])
    }

    /// Enables the co-processor's voltage and current metering.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the write fails.
    pub fn enable_metering<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), I2C::Error> {
        i2c.write(self.address, &[METER_ENABLE, 1])
    }

    /// Reads the battery voltage in millivolts.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the read fails.
    pub fn battery_millivolts<I2C: I2c>(&self, i2c: &mut I2C) -> Result<u16, I2C::Error> {
        let mut value = [0u8; 2];
        i2c.write_read(self.address, &[BATTERY_VOLTAGE], &mut value)?;
        Ok(u16::from_le_bytes(value))
    }

    /// Reads the battery current in milliamps, positive while the battery
    /// charges (the register's sign inverted, as M5Unified does).
    ///
    /// # Errors
    ///
    /// Returns the bus error when the read fails.
    pub fn battery_charge_milliamps<I2C: I2c>(&self, i2c: &mut I2C) -> Result<i32, I2C::Error> {
        let mut value = [0u8; 2];
        i2c.write_read(self.address, &[BATTERY_CURRENT], &mut value)?;
        Ok(i32::from(i16::from_le_bytes(value)).saturating_neg())
    }

    /// Sets every LED's brightness.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the write fails.
    pub fn set_led_brightness<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        brightness: u8,
    ) -> Result<(), I2C::Error> {
        let mut frame = [brightness; 1 + LED_COUNT];
        frame[0] = LED_BRIGHTNESS;
        i2c.write(self.address, &frame)
    }

    /// Shows one red, green, blue color per LED.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the write fails.
    pub fn write_leds<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        colors: &[[u8; 3]; LED_COUNT],
    ) -> Result<(), I2C::Error> {
        let mut frame = [0u8; 1 + 4 * LED_COUNT];
        frame[0] = LED_COLORS;
        for (slot, [red, green, blue]) in frame[1..].as_chunks_mut::<4>().0.iter_mut().zip(colors) {
            slot.copy_from_slice(&[*blue, *green, *red, 0]);
        }
        i2c.write(self.address, &frame)
    }

    /// Reports whether button B is pressed.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the read fails.
    pub fn button_b_pressed<I2C: I2c>(&self, i2c: &mut I2C) -> Result<bool, I2C::Error> {
        let mut value = [0u8; 1];
        i2c.write_read(self.address, &[BUTTONS], &mut value)?;
        Ok(value[0] & BUTTON_B == 0)
    }

    /// Reads and validates the clock.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidClockData`] when the registers hold no valid
    /// date and time, and [`Error::Bus`] when the read fails.
    pub fn read_clock<I2C: I2c>(&self, i2c: &mut I2C) -> Result<ClockData, Error<I2C::Error>> {
        let mut data = [0u8; 7];
        i2c.write_read(self.address, &[CLOCK], &mut data)
            .map_err(Error::Bus)?;
        let weekday = WEEKDAY_CODES
            .iter()
            .position(|code| *code == data[6])
            .and_then(|index| u8::try_from(index).ok())
            .ok_or(Error::InvalidClockData)?;
        let clock = ClockData {
            second: data[0] & 0x3f,
            minute: data[1] & 0x3f,
            hour: data[2] & 0x1f,
            day: data[3] & 0x1f,
            month: data[4] & 0x0f,
            year: 2000_u16.saturating_add(u16::from(data[5] & 0x7f)),
            weekday,
        };
        if valid(clock) {
            Ok(clock)
        } else {
            Err(Error::InvalidClockData)
        }
    }

    /// Writes the clock.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDateTime`] before any transfer when the value
    /// is out of range, and [`Error::Bus`] when the write fails.
    pub fn write_clock<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        clock: ClockData,
    ) -> Result<(), Error<I2C::Error>> {
        if !valid(clock) {
            return Err(Error::InvalidDateTime);
        }
        let year =
            u8::try_from(clock.year.saturating_sub(2000)).map_err(|_| Error::InvalidDateTime)?;
        let weekday = *WEEKDAY_CODES
            .get(usize::from(clock.weekday))
            .ok_or(Error::InvalidDateTime)?;
        i2c.write(
            self.address,
            &[
                CLOCK,
                clock.second,
                clock.minute,
                clock.hour,
                clock.day,
                clock.month,
                year,
                weekday,
            ],
        )
        .map_err(Error::Bus)
    }
}

fn valid(clock: ClockData) -> bool {
    (2000..=2099).contains(&clock.year)
        && (1..=12).contains(&clock.month)
        && (1..=31).contains(&clock.day)
        && clock.weekday <= 6
        && clock.hour <= 23
        && clock.minute <= 59
        && clock.second <= 59
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{ClockData, Error, M5PowerHub, PowerOutput};

    /// A register file with auto-incrementing writes and reads.
    struct Registers([u8; 256]);

    impl ErrorType for Registers {
        type Error = Infallible;
    }

    impl I2c for Registers {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x50);
            let mut register = 0usize;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        register = usize::from(bytes[0]);
                        for (offset, value) in bytes[1..].iter().enumerate() {
                            self.0[register + offset] = *value;
                        }
                    }
                    Operation::Read(values) => {
                        for (offset, value) in values.iter_mut().enumerate() {
                            *value = self.0[register + offset];
                        }
                    }
                }
            }
            Ok(())
        }
    }

    fn hub() -> M5PowerHub {
        M5PowerHub::new(0x50).expect("address")
    }

    #[test]
    fn switches_power_outputs() {
        let mut registers = Registers([0; 256]);
        hub()
            .set_power(&mut registers, PowerOutput::Leds, true)
            .expect("leds");
        hub()
            .set_power(&mut registers, PowerOutput::PortC, true)
            .expect("port c");
        assert_eq!(registers.0[..5], [1, 0, 0, 1, 0]);
        hub().enable_metering(&mut registers).expect("meter");
        assert_eq!(registers.0[5], 1);
    }

    #[test]
    fn reads_battery_voltage_and_charge_current() {
        let mut registers = Registers([0; 256]);
        registers.0[0x30..0x34].copy_from_slice(&[0x1c, 0x20, 0x9c, 0xff]);
        assert_eq!(hub().battery_millivolts(&mut registers), Ok(8220));
        // Raw -100 mA is 100 mA into the battery.
        assert_eq!(hub().battery_charge_milliamps(&mut registers), Ok(100));
    }

    #[test]
    fn writes_led_colors_blue_first_and_brightness() {
        let mut registers = Registers([0; 256]);
        let mut colors = [[0u8; 3]; 8];
        colors[0] = [1, 2, 3];
        colors[7] = [7, 8, 9];
        hub().write_leds(&mut registers, &colors).expect("colors");
        assert_eq!(registers.0[0x60..0x64], [3, 2, 1, 0]);
        assert_eq!(registers.0[0x7c..0x80], [9, 8, 7, 0]);
        hub()
            .set_led_brightness(&mut registers, 63)
            .expect("brightness");
        assert_eq!(registers.0[0x80..0x88], [63; 8]);
    }

    #[test]
    fn button_b_reads_low_while_pressed() {
        let mut registers = Registers([0; 256]);
        registers.0[0xa0] = 0xfe;
        assert_eq!(hub().button_b_pressed(&mut registers), Ok(true));
        registers.0[0xa0] = 0xff;
        assert_eq!(hub().button_b_pressed(&mut registers), Ok(false));
    }

    #[test]
    fn writes_and_reads_the_clock_with_bcd_weekday_codes() {
        let mut registers = Registers([0; 256]);
        let clock = ClockData {
            year: 2026,
            month: 10,
            day: 22,
            weekday: 4,
            hour: 23,
            minute: 59,
            second: 58,
        };
        hub().write_clock(&mut registers, clock).expect("write");
        assert_eq!(registers.0[0xc0..0xc7], [58, 59, 23, 22, 10, 26, 10]);
        assert_eq!(hub().read_clock(&mut registers), Ok(clock));
    }

    #[test]
    fn an_unset_clock_is_invalid() {
        let mut registers = Registers([0; 256]);
        assert_eq!(
            hub().read_clock(&mut registers),
            Err(Error::InvalidClockData)
        );
        let mut bad = ClockData {
            year: 2026,
            month: 13,
            day: 1,
            weekday: 0,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(
            hub().write_clock(&mut registers, bad),
            Err(Error::InvalidDateTime)
        );
        bad.month = 1;
        bad.weekday = 7;
        assert_eq!(
            hub().write_clock(&mut registers, bad),
            Err(Error::InvalidDateTime)
        );
    }
}

//! Low-level Driver for one M5Stack M5IOE1 I2C I/O expander.
//!
//! The M5IOE1 is M5Stack's microcontroller-based expander with fourteen
//! GPIOs (`IO1`..`IO14`) and four 12-bit PWM channels. Its registers, taken
//! from M5Stack's M5Unified `M5IOE1_Class`, keep the low eight pins in a
//! `_L` register and the rest in the following `_H` register:
//!
//! | Register | Address |
//! | --- | --- |
//! | `UID_L`/`UID_H` | `00h`/`01h` |
//! | `GPIO_MODE` (1 = output) | `03h`/`04h` |
//! | `GPIO_OUT` | `05h`/`06h` |
//! | `GPIO_IN` | `07h`/`08h` |
//! | `GPIO_DRV` (1 = open drain) | `13h`/`14h` |
//! | `PWMn_DUTY` (12-bit, bit 15 enable, bit 14 inverted) | `1Bh` + 2(n-1) |
//! | `I2C_CFG` | `23h` |
//! | `PWM_FREQ` (Hz, shared) | `25h`/`26h` |
//!
//! The chip enters an I2C idle sleep that can survive a battery-powered
//! shutdown; [`M5ioe1::wake`] clears it as M5Unified does.

#![no_std]

use embedded_hal::i2c::I2c;

/// Factory I2C address.
pub const DEFAULT_ADDRESS: u8 = 0x4f;

const UID: u8 = 0x00;
const GPIO_MODE: u8 = 0x03;
const GPIO_OUT: u8 = 0x05;
const GPIO_IN: u8 = 0x07;
const GPIO_DRIVE: u8 = 0x13;
const PWM1_DUTY: u8 = 0x1b;
const I2C_CONFIG: u8 = 0x23;
const PWM_FREQUENCY: u8 = 0x25;

/// `I2C_CFG` value with the idle sleep disabled.
const I2C_CONFIG_AWAKE: u8 = 0x00;
/// Highest 12-bit PWM duty.
pub const MAX_DUTY: u16 = 0x0fff;
const PWM_ENABLE: u8 = 1 << 7;
const PWM_INVERTED: u8 = 1 << 6;

/// One of the four PWM channels and the pin it drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PwmChannel {
    /// Channel 1 on `IO9`.
    Ch1,
    /// Channel 2 on `IO8`.
    Ch2,
    /// Channel 3 on `IO11`.
    Ch3,
    /// Channel 4 on `IO10`.
    Ch4,
}

impl PwmChannel {
    const fn duty_register(self) -> u8 {
        match self {
            Self::Ch1 => PWM1_DUTY,
            Self::Ch2 => 0x1d,
            Self::Ch3 => 0x1f,
            Self::Ch4 => 0x21,
        }
    }

    /// Returns the `IO` pin number the channel drives.
    #[must_use]
    pub const fn pin(self) -> u8 {
        match self {
            Self::Ch1 => 9,
            Self::Ch2 => 8,
            Self::Ch3 => 11,
            Self::Ch4 => 10,
        }
    }
}

/// M5IOE1 register or configuration failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The pin is outside `IO1`..`IO14`.
    InvalidPin,
    /// The duty exceeds [`MAX_DUTY`].
    InvalidDuty,
    /// An I2C register operation failed.
    Bus(BusError),
}

/// One addressed M5IOE1 on an I2C bus, owned or borrowed.
pub struct M5ioe1<I2C> {
    i2c: I2C,
    address: u8,
}

impl<I2C> M5ioe1<I2C>
where
    I2C: I2c,
{
    /// Takes one addressed expander.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidAddress`] for a non-seven-bit address.
    pub fn new(i2c: I2C, address: u8) -> Result<Self, Error<I2C::Error>> {
        if address > 0x7f {
            return Err(Error::InvalidAddress);
        }
        Ok(Self { i2c, address })
    }

    /// Reads the chip's unique ID, proving it answers, and disables its I2C
    /// idle sleep.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn wake(&mut self) -> Result<u16, Error<I2C::Error>> {
        let mut uid = [0; 2];
        self.i2c
            .write_read(self.address, &[UID], &mut uid)
            .map_err(Error::Bus)?;
        self.write(I2C_CONFIG, I2C_CONFIG_AWAKE)?;
        Ok(u16::from_le_bytes(uid))
    }

    /// Configures `IO<pin>` as a push-pull output driving `initial_high`.
    /// The level is latched before the pin starts driving.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn configure_output(
        &mut self,
        pin: u8,
        initial_high: bool,
    ) -> Result<(), Error<I2C::Error>> {
        let (offset, mask) = pin_bit(pin)?;
        self.update(GPIO_OUT, offset, |value| {
            set_bits(value, mask, initial_high)
        })?;
        self.update(GPIO_DRIVE, offset, |value| value & !mask)?;
        self.update(GPIO_MODE, offset, |value| value | mask)
    }

    /// Drives a previously configured `IO<pin>` output.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_output(&mut self, pin: u8, high: bool) -> Result<(), Error<I2C::Error>> {
        let (offset, mask) = pin_bit(pin)?;
        self.update(GPIO_OUT, offset, |value| set_bits(value, mask, high))
    }

    /// Reads the level of `IO<pin>`.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn input_is_high(&mut self, pin: u8) -> Result<bool, Error<I2C::Error>> {
        let (offset, mask) = pin_bit(pin)?;
        Ok(self.read(GPIO_IN.wrapping_add(offset))? & mask != 0)
    }

    /// Sets the PWM frequency shared by all four channels.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_pwm_frequency(&mut self, hertz: u16) -> Result<(), Error<I2C::Error>> {
        let [low, high] = hertz.to_le_bytes();
        self.i2c
            .write(self.address, &[PWM_FREQUENCY, low, high])
            .map_err(Error::Bus)
    }

    /// Sets one channel's 12-bit duty, its polarity, and whether it runs.
    /// The channel's pin must be a push-pull output.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDuty`] above [`MAX_DUTY`], or an I2C error.
    pub fn set_pwm_duty(
        &mut self,
        channel: PwmChannel,
        duty: u16,
        inverted: bool,
        enabled: bool,
    ) -> Result<(), Error<I2C::Error>> {
        if duty > MAX_DUTY {
            return Err(Error::InvalidDuty);
        }
        let [low, mut high] = duty.to_le_bytes();
        if enabled {
            high |= PWM_ENABLE;
        }
        if inverted {
            high |= PWM_INVERTED;
        }
        self.i2c
            .write(self.address, &[channel.duty_register(), low, high])
            .map_err(Error::Bus)
    }

    /// Returns the bus.
    pub fn release(self) -> I2C {
        self.i2c
    }

    fn update(
        &mut self,
        register_low: u8,
        offset: u8,
        update: impl FnOnce(u8) -> u8,
    ) -> Result<(), Error<I2C::Error>> {
        let register = register_low.wrapping_add(offset);
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

/// Returns the `_H` register offset and bit mask of `IO<pin>`.
fn pin_bit<BusError>(pin: u8) -> Result<(u8, u8), Error<BusError>> {
    let index = pin
        .checked_sub(1)
        .filter(|index| *index < 14)
        .ok_or(Error::InvalidPin)?;
    Ok((index >> 3, 1 << (index & 7)))
}

const fn set_bits(value: u8, mask: u8, high: bool) -> u8 {
    if high { value | mask } else { value & !mask }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{Error, M5ioe1, PwmChannel};

    /// Register file that records register writes.
    struct Bus {
        registers: [u8; 0x30],
        writes: Vec<Vec<u8>>,
    }

    impl Default for Bus {
        fn default() -> Self {
            Self {
                registers: [0; 0x30],
                writes: Vec::new(),
            }
        }
    }

    impl ErrorType for Bus {
        type Error = core::convert::Infallible;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x4f);
            let mut selected = 0;
            for operation in operations {
                match operation {
                    Operation::Write([register]) => selected = usize::from(*register),
                    Operation::Write(bytes) => {
                        let start = usize::from(bytes[0]);
                        for (offset, value) in bytes[1..].iter().enumerate() {
                            self.registers[start + offset] = *value;
                        }
                        self.writes.push(bytes.to_vec());
                    }
                    Operation::Read(buffer) => {
                        for (offset, value) in buffer.iter_mut().enumerate() {
                            *value = self.registers[selected + offset];
                        }
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn wake_reads_the_uid_and_disables_idle_sleep() {
        let mut bus = Bus::default();
        bus.registers[0] = 0x34;
        bus.registers[1] = 0x12;
        bus.registers[0x23] = 0x80;
        let uid = M5ioe1::new(&mut bus, 0x4f)
            .expect("valid address")
            .wake()
            .expect("wake");
        assert_eq!(uid, 0x1234);
        assert_eq!(bus.registers[0x23], 0);
    }

    #[test]
    fn outputs_latch_their_level_before_driving() {
        let mut bus = Bus::default();
        bus.registers[0x13] = 0xff;
        bus.registers[0x14] = 0xff;
        {
            let mut expander = M5ioe1::new(&mut bus, 0x4f).expect("valid address");
            // IO1: low register, bit 0.
            expander.configure_output(1, true).expect("IO1");
            // IO12: high register, bit 3.
            expander.configure_output(12, false).expect("IO12");
            expander.set_output(12, true).expect("IO12 high");
        }
        assert_eq!(bus.writes[0], [0x05, 0x01]);
        assert_eq!(bus.writes[1], [0x13, 0xfe]);
        assert_eq!(bus.writes[2], [0x03, 0x01]);
        assert_eq!(bus.registers[0x04], 0x08);
        assert_eq!(bus.registers[0x14], 0xf7);
        assert_eq!(bus.registers[0x06], 0x08);
    }

    #[test]
    fn pwm_writes_frequency_and_flagged_duty() {
        let mut bus = Bus::default();
        {
            let mut expander = M5ioe1::new(&mut bus, 0x4f).expect("valid address");
            expander.set_pwm_frequency(1000).expect("frequency");
            expander
                .set_pwm_duty(PwmChannel::Ch3, 0x0abc, false, true)
                .expect("duty");
            expander
                .set_pwm_duty(PwmChannel::Ch1, 0x0001, true, false)
                .expect("duty");
            assert_eq!(
                expander.set_pwm_duty(PwmChannel::Ch1, 0x1000, false, true),
                Err(Error::InvalidDuty)
            );
        }
        assert_eq!(bus.writes[0], [0x25, 0xe8, 0x03]);
        assert_eq!(bus.writes[1], [0x1f, 0xbc, 0x8a]);
        assert_eq!(bus.writes[2], [0x1b, 0x01, 0x40]);
        assert_eq!(PwmChannel::Ch3.pin(), 11);
    }

    #[test]
    fn rejects_invalid_addresses_and_pins() {
        let mut bus = Bus::default();
        assert!(matches!(
            M5ioe1::new(&mut bus, 0x80),
            Err(Error::InvalidAddress)
        ));
        let mut expander = M5ioe1::new(&mut bus, 0x4f).expect("valid address");
        assert_eq!(expander.configure_output(0, true), Err(Error::InvalidPin));
        assert_eq!(expander.set_output(15, true), Err(Error::InvalidPin));
    }
}

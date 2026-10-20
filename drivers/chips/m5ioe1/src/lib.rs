//! Low-level Driver for one M5Stack M5IOE1 I2C I/O expander.
//!
//! The M5IOE1 is M5Stack's microcontroller-based expander with fourteen
//! pins, G1 to G14, four of which can also output PWM. Register addresses and
//! bit layout follow M5Unified's `M5IOE1_Class` (src/utility/M5IOE1_Class.inl,
//! MIT license): every per-pin register is a low/high pair, G1 to G8 in the
//! low byte and G9 to G14 in the high byte.

#![no_std]

use embedded_hal::i2c::I2c;

/// Factory I2C address.
pub const DEFAULT_ADDRESS: u8 = 0x4F;

const GPIO_MODE: u8 = 0x03;
const GPIO_OUTPUT: u8 = 0x05;
const GPIO_INPUT: u8 = 0x07;
const GPIO_DRIVE: u8 = 0x13;
const PWM1_DUTY: u8 = 0x1B;
const I2C_CONFIG: u8 = 0x23;
const PWM_FREQUENCY: u8 = 0x25;
const PWM_ENABLE: u8 = 1 << 7;
const PWM_DUTY_MAX: u16 = 0x0FFF;
const PIN_COUNT: u8 = 14;

/// One of the four PWM outputs and the pin it drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PwmChannel {
    /// PWM on G9.
    G9,
    /// PWM on G8.
    G8,
    /// PWM on G11.
    G11,
    /// PWM on G10.
    G10,
}

impl PwmChannel {
    const fn duty_register(self) -> u8 {
        // Each channel's duty is a low/high byte pair after PWM1's.
        match self {
            Self::G9 => PWM1_DUTY,
            Self::G8 => 0x1D,
            Self::G11 => 0x1F,
            Self::G10 => 0x21,
        }
    }
}

/// M5IOE1 register or configuration failure.
#[derive(Debug)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The pin is not one of G1 to G14.
    InvalidPin,
    /// A PWM duty is above the 12-bit maximum.
    InvalidDuty,
    /// An I2C register operation failed.
    Bus(BusError),
}

/// Borrowed access to one M5IOE1 on a shared I2C bus.
pub struct M5ioe1<'a, I2C> {
    i2c: &'a mut I2C,
    address: u8,
}

impl<'a, I2C> M5ioe1<'a, I2C>
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

    /// Keeps the expander's I2C interface awake at its default 100 kHz, as
    /// M5Unified does before using it.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn disable_i2c_sleep(&mut self) -> Result<(), Error<I2C::Error>> {
        self.write(&[I2C_CONFIG, 0x00])
    }

    /// Makes pin `g` (1 to 14) a push-pull output. Its level is the output
    /// latch, which [`M5ioe1::set_output`] writes.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn configure_output(&mut self, g: u8) -> Result<(), Error<I2C::Error>> {
        let (offset, mask) = pin_bit(g)?;
        self.update(GPIO_DRIVE.wrapping_add(offset), |value| value & !mask)?;
        self.update(GPIO_MODE.wrapping_add(offset), |value| value | mask)
    }

    /// Drives output pin `g` (1 to 14) high or low.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_output(&mut self, g: u8, high: bool) -> Result<(), Error<I2C::Error>> {
        let (offset, mask) = pin_bit(g)?;
        self.update(GPIO_OUTPUT.wrapping_add(offset), |value| {
            if high { value | mask } else { value & !mask }
        })
    }

    /// Reads input pin `g` (1 to 14).
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn input_is_high(&mut self, g: u8) -> Result<bool, Error<I2C::Error>> {
        let (offset, mask) = pin_bit(g)?;
        Ok(self.read(GPIO_INPUT.wrapping_add(offset))? & mask != 0)
    }

    /// Sets the frequency shared by all four PWM outputs.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_pwm_frequency(&mut self, hertz: u16) -> Result<(), Error<I2C::Error>> {
        let [low, high] = hertz.to_le_bytes();
        self.write(&[PWM_FREQUENCY, low, high])
    }

    /// Sets one PWM output's 12-bit duty (0 to 4095) and whether it runs.
    ///
    /// # Errors
    ///
    /// Returns an invalid-duty or I2C register error.
    pub fn set_pwm_duty(
        &mut self,
        channel: PwmChannel,
        duty: u16,
        enabled: bool,
    ) -> Result<(), Error<I2C::Error>> {
        if duty > PWM_DUTY_MAX {
            return Err(Error::InvalidDuty);
        }
        let [low, mut high] = duty.to_le_bytes();
        if enabled {
            high |= PWM_ENABLE;
        }
        self.write(&[channel.duty_register(), low, high])
    }

    fn update(
        &mut self,
        register: u8,
        update: impl FnOnce(u8) -> u8,
    ) -> Result<(), Error<I2C::Error>> {
        let value = self.read(register)?;
        self.write(&[register, update(value)])
    }

    fn read(&mut self, register: u8) -> Result<u8, Error<I2C::Error>> {
        let mut value = [0];
        self.i2c
            .write_read(self.address, &[register], &mut value)
            .map_err(Error::Bus)?;
        Ok(value[0])
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), Error<I2C::Error>> {
        self.i2c.write(self.address, bytes).map_err(Error::Bus)
    }
}

/// Returns which register of a pair holds pin `g` (`0` for G1 to G8, `1` for
/// G9 to G14) and its bit there.
fn pin_bit<BusError>(g: u8) -> Result<(u8, u8), Error<BusError>> {
    if !(1..=PIN_COUNT).contains(&g) {
        return Err(Error::InvalidPin);
    }
    let index = g.saturating_sub(1);
    let offset = u8::from(index >= 8);
    let bit = 1_u8
        .checked_shl(u32::from(index & 7))
        .ok_or(Error::InvalidPin)?;
    Ok((offset, bit))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{Error, GPIO_DRIVE, GPIO_MODE, GPIO_OUTPUT, M5ioe1, PwmChannel};

    struct Bus {
        registers: [u8; 0x30],
    }

    impl Default for Bus {
        fn default() -> Self {
            Self {
                registers: [0; 0x30],
            }
        }
    }

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn read(&mut self, _address: u8, bytes: &mut [u8]) -> Result<(), Self::Error> {
            bytes.fill(0);
            Ok(())
        }

        fn write(&mut self, _address: u8, bytes: &[u8]) -> Result<(), Self::Error> {
            if let [register, values @ ..] = bytes {
                let start = usize::from(*register);
                self.registers[start..start + values.len()].copy_from_slice(values);
            }
            Ok(())
        }

        fn write_read(
            &mut self,
            _address: u8,
            bytes: &[u8],
            read: &mut [u8],
        ) -> Result<(), Self::Error> {
            if let ([register], [value]) = (bytes, read) {
                *value = self.registers[usize::from(*register)];
            }
            Ok(())
        }

        fn transaction(
            &mut self,
            _address: u8,
            _operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn low_and_high_pins_use_their_own_register_of_each_pair() {
        let mut bus = Bus::default();
        bus.registers[usize::from(GPIO_DRIVE)] = 0xff;
        bus.registers[usize::from(GPIO_DRIVE + 1)] = 0xff;
        {
            let mut expander = M5ioe1::new(&mut bus, 0x4f).expect("expander");
            // M5GFX's CoreP4X bring-up: G8 is bit 7 of the low registers,
            // G9 to G12 are bits 0 to 3 of the high ones.
            for g in [8, 9, 12] {
                expander.configure_output(g).expect("output");
                expander.set_output(g, true).expect("high");
            }
            expander.set_output(9, false).expect("low");
        }
        assert_eq!(bus.registers[usize::from(GPIO_DRIVE)], 0x7f);
        assert_eq!(bus.registers[usize::from(GPIO_DRIVE + 1)], 0xf6);
        assert_eq!(bus.registers[usize::from(GPIO_MODE)], 0x80);
        assert_eq!(bus.registers[usize::from(GPIO_MODE + 1)], 0x09);
        assert_eq!(bus.registers[usize::from(GPIO_OUTPUT)], 0x80);
        assert_eq!(bus.registers[usize::from(GPIO_OUTPUT + 1)], 0x08);
    }

    #[test]
    fn pwm_writes_frequency_and_enabled_twelve_bit_duty() {
        let mut bus = Bus::default();
        {
            let mut expander = M5ioe1::new(&mut bus, 0x4f).expect("expander");
            expander.set_pwm_frequency(1_000).expect("frequency");
            expander
                .set_pwm_duty(PwmChannel::G9, 0x0abc, true)
                .expect("duty");
            expander
                .set_pwm_duty(PwmChannel::G10, 0x0123, false)
                .expect("duty");
        }
        // Frequency register 0x25, little-endian.
        assert_eq!(bus.registers[0x25..0x27], [0xe8, 0x03]);
        // PWM1 duty at 0x1B with the enable bit 7 of its high byte.
        assert_eq!(bus.registers[0x1b..0x1d], [0xbc, 0x8a]);
        // PWM4 (G10) three channels later.
        assert_eq!(bus.registers[0x21..0x23], [0x23, 0x01]);
    }

    #[test]
    fn rejects_invalid_addresses_pins_and_duties() {
        let mut bus = Bus::default();
        assert!(matches!(
            M5ioe1::new(&mut bus, 0x80),
            Err(Error::InvalidAddress)
        ));
        let mut expander = M5ioe1::new(&mut bus, 0x4f).expect("expander");
        assert!(matches!(
            expander.configure_output(0),
            Err(Error::InvalidPin)
        ));
        assert!(matches!(
            expander.set_output(15, true),
            Err(Error::InvalidPin)
        ));
        assert!(matches!(
            expander.set_pwm_duty(PwmChannel::G8, 0x1000, true),
            Err(Error::InvalidDuty)
        ));
    }
}

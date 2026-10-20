//! Low-level Driver for the M5Stack M5PM1 power-management controller.
//!
//! The M5PM1 is the microcontroller M5Stack's "Stamp Timer Power2" uses for
//! charging, power rails, and wake sources. Registers and bits follow
//! M5Unified's `M5PM1_Class` (src/utility/power/M5PM1_Class.inl, MIT
//! license). This driver covers the rail and housekeeping registers and the
//! five general-purpose pins G0 to G4 with their PWM function, which M5Stack's
//! Paper boards use for the e-paper supply and the front light; the charger
//! and wake functions are left out until a consumer needs them.

#![no_std]

use embedded_hal::i2c::I2c;

/// Factory I2C address.
pub const DEFAULT_ADDRESS: u8 = 0x6E;

const POWER_CONFIG: u8 = 0x06;
const I2C_CONFIG: u8 = 0x09;
const WATCHDOG_COUNT: u8 = 0x0A;
const GPIO_MODE: u8 = 0x10;
const GPIO_OUTPUT: u8 = 0x11;
const GPIO_DRIVE: u8 = 0x13;
const GPIO_FUNCTION_LOW: u8 = 0x16;
const GPIO_FUNCTION_HIGH: u8 = 0x17;
const PWM0_DUTY: u8 = 0x30;
const PWM_FREQUENCY: u8 = 0x34;

/// Number of general-purpose pins, G0 to G4.
pub const GPIO_COUNT: u8 = 5;
/// Largest 12-bit PWM duty.
pub const PWM_DUTY_MAX: u16 = 0x0FFF;
/// PWM enable bit in a duty register's high byte.
const PWM_ENABLE: u8 = 1 << 4;

/// Function of one general-purpose pin (two bits in `GPIO_FUNC0/1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinFunction {
    /// Plain input or output.
    Gpio = 0b00,
    /// The pin's special function; PWM on the PWM-capable pins.
    Special = 0b11,
}

/// One power rail switched in the power-configuration register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rail {
    /// The DC/DC converter.
    DcDc,
    /// The LDO.
    Ldo,
    /// The 5 V boost converter.
    Boost,
}

impl Rail {
    const fn mask(self) -> u8 {
        match self {
            Self::DcDc => 1 << 1,
            Self::Ldo => 1 << 2,
            Self::Boost => 1 << 3,
        }
    }
}

/// M5PM1 register or configuration failure.
#[derive(Debug)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The pin is outside G0 to G4.
    InvalidPin,
    /// The PWM channel or duty is out of range.
    InvalidDuty,
    /// An I2C register operation failed.
    Bus(BusError),
}

/// Borrowed access to one M5PM1 on a shared I2C bus.
pub struct M5pm1<'a, I2C> {
    i2c: &'a mut I2C,
    address: u8,
}

impl<'a, I2C> M5pm1<'a, I2C>
where
    I2C: I2c,
{
    /// Borrows one addressed controller.
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

    /// Keeps the controller's I2C interface awake, as M5Unified does before
    /// using it.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn disable_i2c_sleep(&mut self) -> Result<(), Error<I2C::Error>> {
        self.write(I2C_CONFIG, 0x00)
    }

    /// Stops the controller's watchdog.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn disable_watchdog(&mut self) -> Result<(), Error<I2C::Error>> {
        self.write(WATCHDOG_COUNT, 0x00)
    }

    /// Switches one rail on or off without touching the others.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_rail_enabled(&mut self, rail: Rail, enabled: bool) -> Result<(), Error<I2C::Error>> {
        let value = self.read(POWER_CONFIG)?;
        let mask = rail.mask();
        self.write(
            POWER_CONFIG,
            if enabled { value | mask } else { value & !mask },
        )
    }

    /// Selects the function of pin `g` (0 to 4).
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_pin_function(
        &mut self,
        g: u8,
        function: PinFunction,
    ) -> Result<(), Error<I2C::Error>> {
        if g >= GPIO_COUNT {
            return Err(Error::InvalidPin);
        }
        let (register, shift) = if g < 4 {
            (GPIO_FUNCTION_LOW, g.wrapping_mul(2))
        } else {
            (GPIO_FUNCTION_HIGH, 0)
        };
        let mask = 0b11_u8 << shift;
        let bits = (function as u8) << shift;
        self.update(register, |value| (value & !mask) | bits)
    }

    /// Makes pin `g` (0 to 4) a push-pull GPIO output. Its level is the output
    /// latch, which [`M5pm1::set_output`] writes.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn configure_output(&mut self, g: u8) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(g)?;
        self.set_pin_function(g, PinFunction::Gpio)?;
        self.update(GPIO_MODE, |value| value | mask)?;
        self.update(GPIO_DRIVE, |value| value & !mask)
    }

    /// Drives output pin `g` (0 to 4) high or low.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_output(&mut self, g: u8, high: bool) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(g)?;
        self.update(
            GPIO_OUTPUT,
            |value| {
                if high { value | mask } else { value & !mask }
            },
        )
    }

    /// Hands pin `g` (0 to 4) to its PWM function with a push-pull driver.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn configure_pwm_output(&mut self, g: u8) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(g)?;
        self.update(GPIO_DRIVE, |value| value & !mask)?;
        self.set_pin_function(g, PinFunction::Special)
    }

    /// Sets the frequency shared by the PWM outputs.
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

    /// Sets PWM channel `channel` (0 or 1) to a 12-bit duty and whether it
    /// runs.
    ///
    /// # Errors
    ///
    /// Returns an invalid-duty or I2C register error.
    pub fn set_pwm_duty(
        &mut self,
        channel: u8,
        duty: u16,
        enabled: bool,
    ) -> Result<(), Error<I2C::Error>> {
        if duty > PWM_DUTY_MAX || channel > 1 {
            return Err(Error::InvalidDuty);
        }
        let [low, mut high] = duty.to_le_bytes();
        if enabled {
            high |= PWM_ENABLE;
        }
        let register = PWM0_DUTY.wrapping_add(channel.wrapping_mul(2));
        self.i2c
            .write(self.address, &[register, low, high])
            .map_err(Error::Bus)
    }

    /// Stops PWM channel `channel` (0 or 1) by clearing its duty's high byte,
    /// as M5GFX's Paper Mono front light does for brightness zero.
    ///
    /// # Errors
    ///
    /// Returns an invalid-duty or I2C register error.
    pub fn stop_pwm(&mut self, channel: u8) -> Result<(), Error<I2C::Error>> {
        if channel > 1 {
            return Err(Error::InvalidDuty);
        }
        let register = PWM0_DUTY
            .wrapping_add(channel.wrapping_mul(2))
            .wrapping_add(1);
        self.write(register, 0x00)
    }

    fn update(
        &mut self,
        register: u8,
        update: impl FnOnce(u8) -> u8,
    ) -> Result<(), Error<I2C::Error>> {
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

fn pin_mask<BusError>(g: u8) -> Result<u8, Error<BusError>> {
    if g >= GPIO_COUNT {
        return Err(Error::InvalidPin);
    }
    1_u8.checked_shl(u32::from(g)).ok_or(Error::InvalidPin)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{Error, M5pm1, Rail};

    #[test]
    fn epd_supply_pin_matches_m5gfx_paper_diy_bring_up() {
        // M5GFX board_M5PaperDIY: 0x16 &= !(0b11 << 4), 0x10 |= 1 << 2,
        // 0x13 &= !(1 << 2), 0x11 |= 1 << 2.
        let mut bus = Bus {
            registers: [0xff; 0x40],
        };
        bus.registers[0x10] = 0x00;
        bus.registers[0x11] = 0x00;
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller.configure_output(2).expect("G2 output");
            controller.set_output(2, true).expect("G2 high");
        }
        assert_eq!(bus.registers[0x16], 0xcf);
        assert_eq!(bus.registers[0x10], 0x04);
        assert_eq!(bus.registers[0x13], 0xfb);
        assert_eq!(bus.registers[0x11], 0x04);
    }

    #[test]
    fn front_light_pwm_matches_m5gfx_paper_mono() {
        // M5GFX Light_M5PaperMono: G3 push-pull, 0x16 |= 0xC0, 5 kHz, then a
        // squared 12-bit duty with the enable bit.
        let mut bus = Bus {
            registers: [0x00; 0x40],
        };
        bus.registers[0x13] = 0xff;
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller.configure_pwm_output(3).expect("G3 PWM");
            controller.set_pwm_frequency(5_000).expect("frequency");
            controller.set_pwm_duty(0, 0x0fe0, true).expect("duty");
        }
        assert_eq!(bus.registers[0x13], 0xf7);
        assert_eq!(bus.registers[0x16], 0xc0);
        assert_eq!(&bus.registers[0x34..0x36], &[0x88, 0x13]);
        assert_eq!(&bus.registers[0x30..0x32], &[0xe0, 0x1f]);
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller.stop_pwm(0).expect("stop");
            assert!(matches!(
                controller.configure_output(5),
                Err(Error::InvalidPin)
            ));
            assert!(matches!(
                controller.set_pwm_duty(0, 0x1000, true),
                Err(Error::InvalidDuty)
            ));
        }
        assert_eq!(bus.registers[0x31], 0x00);
    }

    struct Bus {
        registers: [u8; 0x40],
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
                for (offset, value) in values.iter().enumerate() {
                    self.registers[usize::from(*register) + offset] = *value;
                }
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
    fn housekeeping_and_rails_match_m5gfx_corep4x_bring_up() {
        // M5GFX's CoreP4X bring-up writes 0x09 = 0, 0x0A = 0 and sets bit 3
        // of 0x06; the charger bit 0 stays as it was.
        let mut bus = Bus {
            registers: [0xff; 0x40],
        };
        bus.registers[0x06] = 0x01;
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller.disable_i2c_sleep().expect("I2C sleep");
            controller.disable_watchdog().expect("watchdog");
            controller
                .set_rail_enabled(Rail::Boost, true)
                .expect("boost on");
            controller
                .set_rail_enabled(Rail::Ldo, true)
                .expect("LDO on");
            controller
                .set_rail_enabled(Rail::Ldo, false)
                .expect("LDO off");
        }
        assert_eq!(bus.registers[0x09], 0x00);
        assert_eq!(bus.registers[0x0a], 0x00);
        assert_eq!(bus.registers[0x06], 0x09);
    }

    #[test]
    fn rejects_an_invalid_address() {
        let mut bus = Bus {
            registers: [0; 0x40],
        };
        assert!(matches!(
            M5pm1::new(&mut bus, 0x80),
            Err(Error::InvalidAddress)
        ));
    }
}

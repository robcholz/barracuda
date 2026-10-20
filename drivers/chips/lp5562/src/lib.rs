//! Low-level Driver for one TI LP5562 four-channel I2C LED driver.
//!
//! The LP5562 drives red, green, blue, and white current sinks, each with an
//! eight-bit PWM duty and an eight-bit output current. This driver controls
//! the channels directly through their I2C registers; the internal program
//! engines are left unused.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// `ENABLE` register: chip enable, logarithmic dimming, and engine control.
const ENABLE: u8 = 0x00;
/// `CONFIG` register: PWM frequency, power save, and clock selection.
const CONFIG: u8 = 0x08;
/// `RESET` register: writing [`RESET_VALUE`] restores every register default.
const RESET: u8 = 0x0d;
/// `LED_MAP` register: selects which source drives each output.
const LED_MAP: u8 = 0x70;

/// `ENABLE.CHIP_EN`.
const CHIP_EN: u8 = 1 << 6;
/// `ENABLE.LOG_EN`.
const LOG_EN: u8 = 1 << 7;
/// `CONFIG.PWM_HF`: 558 Hz instead of 256 Hz PWM.
const PWM_HF: u8 = 1 << 6;
/// `CONFIG.INT_CLK_EN`: use the internal oscillator.
const INT_CLK_EN: u8 = 1 << 0;
/// `LED_MAP` value placing all four outputs under direct I2C register control.
const ALL_I2C_CONTROLLED: u8 = 0x00;
/// Value that resets the chip when written to `RESET`.
const RESET_VALUE: u8 = 0xff;
/// Start-up time after `CHIP_EN` before other registers may be written
/// (datasheet: 500 µs; rounded up).
const STARTUP_DELAY_US: u32 = 1_000;

/// One LP5562 output channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Blue output (`B`).
    Blue,
    /// Green output (`G`).
    Green,
    /// Red output (`R`).
    Red,
    /// White output (`W`).
    White,
}

impl Channel {
    /// Returns the channel's PWM duty register.
    const fn pwm_register(self) -> u8 {
        match self {
            Self::Blue => 0x02,
            Self::Green => 0x03,
            Self::Red => 0x04,
            Self::White => 0x0e,
        }
    }

    /// Returns the channel's output current register.
    const fn current_register(self) -> u8 {
        match self {
            Self::Blue => 0x05,
            Self::Green => 0x06,
            Self::Red => 0x07,
            Self::White => 0x0f,
        }
    }
}

/// Clock and PWM options applied by [`Lp5562::enable`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Use 558 Hz instead of 256 Hz PWM.
    pub high_frequency_pwm: bool,
    /// Map PWM duty logarithmically instead of linearly.
    pub logarithmic_dimming: bool,
}

/// LP5562 register or configuration failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// An I2C register write failed.
    Bus(BusError),
}

/// One addressed LP5562 on an I2C bus.
///
/// The bus may be owned or borrowed: `embedded-hal` implements [`I2c`] for
/// `&mut` references as well.
pub struct Lp5562<I2C> {
    i2c: I2C,
    address: u8,
}

impl<I2C> Lp5562<I2C>
where
    I2C: I2c,
{
    /// Takes one addressed LP5562.
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

    /// Enables the chip on its internal clock and places every output under
    /// direct register control.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn enable<D: DelayNs>(
        &mut self,
        options: Options,
        delay: &mut D,
    ) -> Result<(), Error<I2C::Error>> {
        let enable = if options.logarithmic_dimming {
            CHIP_EN | LOG_EN
        } else {
            CHIP_EN
        };
        self.write(ENABLE, enable)?;
        delay.delay_us(STARTUP_DELAY_US);
        let config = if options.high_frequency_pwm {
            PWM_HF | INT_CLK_EN
        } else {
            INT_CLK_EN
        };
        self.write(CONFIG, config)?;
        self.write(LED_MAP, ALL_I2C_CONTROLLED)
    }

    /// Stops the chip; every output turns off until [`Self::enable`].
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn disable(&mut self) -> Result<(), Error<I2C::Error>> {
        self.write(ENABLE, 0)
    }

    /// Restores every register to its power-on default.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn reset(&mut self) -> Result<(), Error<I2C::Error>> {
        self.write(RESET, RESET_VALUE)
    }

    /// Sets one channel's PWM duty, `0` off through `255` fully on.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_pwm(&mut self, channel: Channel, duty: u8) -> Result<(), Error<I2C::Error>> {
        self.write(channel.pwm_register(), duty)
    }

    /// Sets one channel's output current in 100 µA steps (`0xaf` = 17.5 mA
    /// after reset).
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_current(
        &mut self,
        channel: Channel,
        tenths_of_milliamp: u8,
    ) -> Result<(), Error<I2C::Error>> {
        self.write(channel.current_register(), tenths_of_milliamp)
    }

    /// Returns the bus.
    pub fn release(self) -> I2C {
        self.i2c
    }

    fn write(&mut self, register: u8, value: u8) -> Result<(), Error<I2C::Error>> {
        self.i2c
            .write(self.address, &[register, value])
            .map_err(Error::Bus)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use embedded_hal::{
        delay::DelayNs,
        i2c::{self, ErrorType, I2c, Operation},
    };

    use super::{Channel, Error, Lp5562, Options};

    #[derive(Default)]
    struct Bus {
        writes: Vec<(u8, u8, u8)>,
        fail: bool,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct BusError;

    impl i2c::Error for BusError {
        fn kind(&self) -> i2c::ErrorKind {
            i2c::ErrorKind::Other
        }
    }

    impl ErrorType for Bus {
        type Error = BusError;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            if self.fail {
                return Err(BusError);
            }
            for operation in operations {
                if let Operation::Write([register, value]) = operation {
                    self.writes.push((address, *register, *value));
                }
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay {
        elapsed_ns: u64,
    }

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.elapsed_ns = self.elapsed_ns.saturating_add(u64::from(ns));
        }
    }

    #[test]
    fn enable_waits_for_startup_and_maps_outputs_to_registers() {
        let mut bus = Bus::default();
        let mut delay = Delay::default();
        Lp5562::new(&mut bus, 0x30)
            .expect("valid address")
            .enable(Options::default(), &mut delay)
            .expect("enable");
        assert_eq!(
            bus.writes,
            [(0x30, 0x00, 0x40), (0x30, 0x08, 0x01), (0x30, 0x70, 0x00)]
        );
        assert!(delay.elapsed_ns >= 500_000);
    }

    #[test]
    fn options_select_logarithmic_dimming_and_high_frequency_pwm() {
        let mut bus = Bus::default();
        Lp5562::new(&mut bus, 0x30)
            .expect("valid address")
            .enable(
                Options {
                    high_frequency_pwm: true,
                    logarithmic_dimming: true,
                },
                &mut Delay::default(),
            )
            .expect("enable");
        assert_eq!(bus.writes[0], (0x30, 0x00, 0xc0));
        assert_eq!(bus.writes[1], (0x30, 0x08, 0x41));
    }

    #[test]
    fn channels_address_their_pwm_and_current_registers() {
        let mut bus = Bus::default();
        {
            let mut chip = Lp5562::new(&mut bus, 0x30).expect("valid address");
            chip.set_pwm(Channel::White, 0x80).expect("white pwm");
            chip.set_pwm(Channel::Red, 1).expect("red pwm");
            chip.set_pwm(Channel::Green, 2).expect("green pwm");
            chip.set_pwm(Channel::Blue, 3).expect("blue pwm");
            chip.set_current(Channel::White, 0xaf)
                .expect("white current");
            chip.set_current(Channel::Blue, 0x10).expect("blue current");
            chip.disable().expect("disable");
            chip.reset().expect("reset");
        }
        assert_eq!(
            bus.writes,
            [
                (0x30, 0x0e, 0x80),
                (0x30, 0x04, 1),
                (0x30, 0x03, 2),
                (0x30, 0x02, 3),
                (0x30, 0x0f, 0xaf),
                (0x30, 0x05, 0x10),
                (0x30, 0x00, 0x00),
                (0x30, 0x0d, 0xff),
            ]
        );
    }

    #[test]
    fn rejects_invalid_address_and_reports_bus_errors() {
        assert!(matches!(
            Lp5562::new(Bus::default(), 0x80),
            Err(Error::InvalidAddress)
        ));
        let mut chip = Lp5562::new(
            Bus {
                fail: true,
                ..Bus::default()
            },
            0x30,
        )
        .expect("valid address");
        assert_eq!(chip.set_pwm(Channel::White, 1), Err(Error::Bus(BusError)));
    }
}

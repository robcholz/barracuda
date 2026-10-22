//! Vibration motor on PWM channel 1 of an M5Stack M5IOE1 expander.
//!
//! The M5Stack StopWatch drives its motor from the M5IOE1's `IO9`. Start-up
//! follows M5Unified `Power_Class::begin` (board_M5StopWatch): the channel
//! off, the shared PWM frequency at 2 kHz, and `IO9` a push-pull output.
//! The strength is the 12-bit duty, as `Power_Class::setVibration` sets it
//! (`level × 0xFFF / 255`); zero disables the channel.
//!
//! # Shared ownership
//!
//! The display and audio codec use other pins of the same expander through
//! their own views of the bus. This implementation writes the channel 1 duty
//! registers (`1Bh`/`1Ch`), the PWM frequency (`25h`/`26h`), which no other
//! implementation on these Boards uses, and only `IO9`'s bits of the shared
//! output, drive, and mode registers.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_m5ioe1::{Error as ExpanderError, M5ioe1, MAX_DUTY, PwmChannel};
use barracuda_peripheral::{
    PeripheralImplementation,
    vibration::{Pulse, Strength, Vibration},
};
use embassy_time::Timer;
use embedded_hal::i2c::I2c;

/// The motor's channel; it drives `IO9`.
const MOTOR_CHANNEL: PwmChannel = PwmChannel::Ch1;
/// M5Unified `motor_pwm_hz`.
const MOTOR_PWM_HZ: u16 = 2_000;

/// Board-owned expander address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct M5ioe1VibrationConfig {
    address: u8,
}

impl M5ioe1VibrationConfig {
    /// Creates configuration for the expander at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Initialized expander-driven vibration motor.
pub struct M5ioe1Vibration<I2C> {
    i2c: I2C,
    address: u8,
}

/// Static factory used by generated Board composition.
pub struct M5ioe1VibrationImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for M5ioe1VibrationImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = I2C;
    type Config = M5ioe1VibrationConfig;
    type Peripheral = M5ioe1Vibration<I2C>;
    type Error = ExpanderError<I2C::Error>;

    async fn initialize(i2c: I2C, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let mut i2c = i2c;
        let mut expander = M5ioe1::new(&mut i2c, config.address)?;
        expander.set_pwm_duty(MOTOR_CHANNEL, 0, false, false)?;
        expander.set_pwm_frequency(MOTOR_PWM_HZ)?;
        expander.configure_output(MOTOR_CHANNEL.pin(), false)?;
        Ok(M5ioe1Vibration {
            i2c,
            address: config.address,
        })
    }
}

/// M5Unified `setVibration`: `level × 0xFFF / 255` for level 0 to 255.
fn duty(strength: Strength) -> u16 {
    let level = u32::from(strength.get()) * 255 / 100;
    u16::try_from(level * u32::from(MAX_DUTY) / 255).unwrap_or(MAX_DUTY)
}

/// Stops the motor when dropped.
struct StopOnDrop<'a, I2C: I2c>(&'a mut M5ioe1Vibration<I2C>);

impl<I2C: I2c> Drop for StopOnDrop<'_, I2C> {
    fn drop(&mut self) {
        // Best effort on cancellation; a completed pattern reports its error.
        let _ = self.0.set(Strength::OFF);
    }
}

impl<I2C: I2c> Vibration for M5ioe1Vibration<I2C> {
    type Error = ExpanderError<I2C::Error>;

    fn has_variable_strength(&self) -> bool {
        true
    }

    fn set(&mut self, strength: Strength) -> Result<(), Self::Error> {
        let mut expander = M5ioe1::new(&mut self.i2c, self.address)?;
        expander.set_pwm_duty(MOTOR_CHANNEL, duty(strength), false, !strength.is_off())
    }

    async fn pulse(&mut self, pattern: &[Pulse], strength: Strength) -> Result<(), Self::Error> {
        let motor = StopOnDrop(self);
        for step in pattern {
            motor.0.set(strength)?;
            Timer::after_millis(u64::from(step.on_ms())).await;
            motor.0.set(Strength::OFF)?;
            Timer::after_millis(u64::from(step.off_ms())).await;
        }
        motor.0.set(Strength::OFF)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_peripheral::{
        PeripheralImplementation,
        vibration::{Pulse, Strength, Vibration},
    };
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{M5ioe1VibrationConfig, M5ioe1VibrationImplementation};

    /// Register file with auto-incrementing multi-byte writes.
    struct Expander {
        registers: [u8; 0x40],
    }

    impl ErrorType for Expander {
        type Error = Infallible;
    }

    impl I2c for Expander {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut register = 0usize;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        if let Some((first, values)) = bytes.split_first() {
                            register = usize::from(*first);
                            for (offset, value) in values.iter().enumerate() {
                                self.registers[register + offset] = *value;
                            }
                        }
                    }
                    Operation::Read(values) => {
                        for (offset, value) in values.iter_mut().enumerate() {
                            *value = self.registers[register + offset];
                        }
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn starts_silent_at_two_kilohertz_with_io9_driving() {
        let motor = block_on(M5ioe1VibrationImplementation::initialize(
            Expander {
                registers: [0; 0x40],
            },
            M5ioe1VibrationConfig::new(0x4f),
        ))
        .expect("start");
        let registers = motor.i2c.registers;
        assert_eq!([registers[0x25], registers[0x26]], 2_000u16.to_le_bytes());
        // IO9 is bit 0 of the high mode register, push-pull.
        assert_eq!(registers[0x04] & 1, 1);
        assert_eq!(registers[0x14] & 1, 0);
        assert_eq!(registers[0x1c] & 0x80, 0);
    }

    #[test]
    fn strength_is_the_twelve_bit_duty_and_a_pattern_ends_stopped() {
        let mut motor = block_on(M5ioe1VibrationImplementation::initialize(
            Expander {
                registers: [0; 0x40],
            },
            M5ioe1VibrationConfig::new(0x4f),
        ))
        .expect("start");
        motor.set(Strength::MAX).expect("on");
        assert_eq!(
            [motor.i2c.registers[0x1b], motor.i2c.registers[0x1c]],
            [0xff, 0x8f]
        );
        block_on(motor.pulse(&[Pulse::new(1, 1)], Strength::MAX)).expect("pulse");
        assert_eq!(motor.i2c.registers[0x1c] & 0x80, 0);
    }
}

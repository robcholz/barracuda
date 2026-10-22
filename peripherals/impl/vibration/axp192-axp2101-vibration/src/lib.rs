//! Vibration motor fed by an AXP192 or AXP2101 PMIC regulator.
//!
//! The M5Stack Core2 powers its motor from AXP192 LDO3 on its first revision
//! and from AXP2101 DLDO1 on v1.1, both at `34h`; the chip ID tells them
//! apart at start, as M5Unified `Power_Class::begin` does. The strength sets
//! the regulator voltage as M5Unified `Power_Class::setVibration` does:
//! `480 + level × 12` mV for a level of 1 to 255, rounded down to the
//! regulator's 100 mV steps within its range; zero switches it off.
//!
//! # Shared ownership
//!
//! `m5stack-pmic-display` and `axp192-axp2101-power-monitor` use the same
//! PMIC through their own views of the bus. This implementation writes only
//! the motor regulator's voltage field (AXP192 `28h` bits 3:0, AXP2101
//! `99h`) and its enable bit (AXP192 `12h` bit 3, AXP2101 `90h` bit 7),
//! each a read-modify-write that leaves the other bits as they are.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_axp192::{Axp192, Error as Axp192Error, Rail};
use barracuda_driver_axp2101::{Axp2101, Error as Axp2101Error, Ldo};
use barracuda_peripheral::{
    PeripheralImplementation,
    vibration::{Pulse, Strength, Vibration},
};
use embassy_time::Timer;
use embedded_hal::i2c::I2c;

/// Board-owned PMIC address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxpVibrationConfig {
    address: u8,
}

impl AxpVibrationConfig {
    /// Creates configuration for the PMIC at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Which PMIC feeds the motor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pmic {
    /// X-Powers AXP192, LDO3.
    Axp192,
    /// X-Powers AXP2101, DLDO1.
    Axp2101,
}

/// Register failure.
#[derive(Debug)]
pub enum AxpVibrationError<BusError> {
    /// No AXP192 or AXP2101 answered with its chip ID.
    UnsupportedPmic,
    /// An AXP192 register operation failed.
    Axp192(Axp192Error<BusError>),
    /// An AXP2101 register operation failed.
    Axp2101(Axp2101Error<BusError>),
}

/// Initialized PMIC-fed vibration motor.
pub struct AxpVibration<I2C> {
    i2c: I2C,
    address: u8,
    pmic: Pmic,
}

/// Static factory used by generated Board composition.
pub struct AxpVibrationImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for AxpVibrationImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = I2C;
    type Config = AxpVibrationConfig;
    type Peripheral = AxpVibration<I2C>;
    type Error = AxpVibrationError<I2C::Error>;

    async fn initialize(i2c: I2C, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let mut i2c = i2c;
        let address = config.address;
        // Both PMICs keep their ID in REG03H.
        let pmic = if Axp192::new(&mut i2c, address)
            .and_then(|mut pmic| pmic.is_present())
            .map_err(AxpVibrationError::Axp192)?
        {
            Pmic::Axp192
        } else if Axp2101::new(&mut i2c, address)
            .and_then(|mut pmic| pmic.is_present())
            .map_err(AxpVibrationError::Axp2101)?
        {
            Pmic::Axp2101
        } else {
            return Err(AxpVibrationError::UnsupportedPmic);
        };
        let mut motor = AxpVibration { i2c, address, pmic };
        motor.set(Strength::OFF)?;
        Ok(motor)
    }
}

impl<I2C> AxpVibration<I2C> {
    /// Returns the PMIC that answered at initialization.
    #[must_use]
    pub const fn pmic(&self) -> Pmic {
        self.pmic
    }
}

/// M5Unified `setVibration`: `480 + level × 12` mV for level 1 to 255,
/// rounded down to 100 mV steps from `minimum` to `maximum`.
fn regulator_millivolts(strength: Strength, minimum: u16, maximum: u16) -> u16 {
    let level = u32::from(strength.get()) * 255 / 100;
    let millivolts = 480 + level * 12;
    let clamped = millivolts.clamp(u32::from(minimum), u32::from(maximum));
    let stepped = clamped - (clamped - u32::from(minimum)) % 100;
    u16::try_from(stepped).unwrap_or(maximum)
}

/// Stops the motor when dropped.
struct StopOnDrop<'a, I2C: I2c>(&'a mut AxpVibration<I2C>);

impl<I2C: I2c> Drop for StopOnDrop<'_, I2C> {
    fn drop(&mut self) {
        // Best effort on cancellation; a completed pattern reports its error.
        let _ = self.0.set(Strength::OFF);
    }
}

impl<I2C: I2c> Vibration for AxpVibration<I2C> {
    type Error = AxpVibrationError<I2C::Error>;

    fn has_variable_strength(&self) -> bool {
        true
    }

    fn set(&mut self, strength: Strength) -> Result<(), Self::Error> {
        let on = !strength.is_off();
        match self.pmic {
            Pmic::Axp192 => {
                let mut pmic =
                    Axp192::new(&mut self.i2c, self.address).map_err(AxpVibrationError::Axp192)?;
                if on {
                    pmic.set_millivolts(Rail::Ldo3, regulator_millivolts(strength, 1_800, 3_300))
                        .map_err(AxpVibrationError::Axp192)?;
                }
                pmic.set_enabled(Rail::Ldo3, on)
                    .map_err(AxpVibrationError::Axp192)
            }
            Pmic::Axp2101 => {
                let mut pmic = Axp2101::new(&mut self.i2c, self.address)
                    .map_err(AxpVibrationError::Axp2101)?;
                if on {
                    pmic.set_millivolts(Ldo::Dldo1, regulator_millivolts(strength, 500, 3_300))
                        .map_err(AxpVibrationError::Axp2101)?;
                }
                pmic.set_enabled(Ldo::Dldo1, on)
                    .map_err(AxpVibrationError::Axp2101)
            }
        }
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_peripheral::{
        PeripheralImplementation,
        vibration::{Pulse, Strength, Vibration},
    };
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{AxpVibrationConfig, AxpVibrationImplementation, regulator_millivolts};

    /// Register file at 34h answering with the given IC type.
    struct Pmic {
        registers: [u8; 0x100],
    }

    impl Pmic {
        fn new(ic_type: u8) -> Self {
            let mut registers = [0; 0x100];
            registers[0x03] = ic_type;
            // Display rails already on.
            registers[0x12] = 0b0000_0110;
            registers[0x28] = 0xc0;
            registers[0x90] = 0b0001_0010;
            Self { registers }
        }
    }

    impl ErrorType for Pmic {
        type Error = Infallible;
    }

    impl I2c for Pmic {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = *reg,
                    Operation::Write([reg, value]) => self.registers[usize::from(*reg)] = *value,
                    Operation::Read([value]) => *value = self.registers[usize::from(register)],
                    _ => {}
                }
            }
            Ok(())
        }
    }

    #[test]
    fn strength_follows_m5unified_set_vibration() {
        // Level 255: 480 + 3060 mV, capped.
        assert_eq!(regulator_millivolts(Strength::MAX, 1_800, 3_300), 3_300);
        // 50 % is level 127: 2004 mV, down to 2000.
        assert_eq!(
            regulator_millivolts(Strength::percent(50), 500, 3_300),
            2_000
        );
        assert_eq!(
            regulator_millivolts(Strength::percent(1), 1_800, 3_300),
            1_800
        );
    }

    #[test]
    fn axp192_drives_ldo3_and_leaves_the_display_rails() {
        let mut motor = block_on(AxpVibrationImplementation::initialize(
            Pmic::new(0x03),
            AxpVibrationConfig::new(0x34),
        ))
        .expect("start");
        motor.set(Strength::MAX).expect("on");
        assert_eq!(motor.i2c.registers[0x12], 0b0000_1110);
        assert_eq!(motor.i2c.registers[0x28], 0xcf);
        motor.set(Strength::OFF).expect("off");
        assert_eq!(motor.i2c.registers[0x12], 0b0000_0110);
    }

    #[test]
    fn axp2101_drives_dldo1_and_a_pattern_ends_stopped() {
        let mut motor = block_on(AxpVibrationImplementation::initialize(
            Pmic::new(0x4a),
            AxpVibrationConfig::new(0x34),
        ))
        .expect("start");
        block_on(motor.pulse(&[Pulse::new(1, 1)], Strength::percent(50))).expect("pulse");
        assert_eq!(motor.i2c.registers[0x99], 15);
        assert_eq!(motor.i2c.registers[0x90], 0b0001_0010);
        motor.set(Strength::MAX).expect("on");
        assert_eq!(motor.i2c.registers[0x90], 0b1001_0010);
    }
}

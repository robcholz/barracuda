//! The power key of the M5Stack M5PM1 power controller as one button.
//!
//! M5Stack's StickS3, StopWatch, CoreP4X, Paper boards, and StampS3-Bat wire
//! their power key to the M5PM1's PWR_BTN input. The M5PM1 reports the key's
//! level in `BTN_STATUS` (`48h`) bit 0 and records a press in bit 7, which
//! clears when read (M5Stack's M5PM1 library, `btnGetState` and
//! `btnGetFlag`). This implementation reports the key as button 0, pressed
//! while it is down and in the first [`Buttons::read`] after a press too short
//! to fall between two reads.
//!
//! M5Unified `M5PM1_Class::getPekPress` reads the single- and double-click
//! interrupts in `42h` instead. The M5PM1 clears those by itself unless one
//! of its pins is configured as the interrupt output (M5Unified
//! `Power_Class::begin`), and each Board uses its pins for something else, so
//! this implementation leaves the interrupt registers alone.
//!
//! With its factory settings the M5PM1 resets the system on a single click
//! and turns it off on a double click (M5PM1 user manual, `BTN_CFG_1` and
//! `BTN_CFG_2`). [`SingleClickReset::Disable`] clears the first so a click
//! reaches the application.
//!
//! # Shared ownership
//!
//! Display and battery-monitor implementations use the same M5PM1 through
//! their own views of the bus. This implementation reads only `BTN_STATUS`,
//! writes only the single-click-reset bit of `BTN_CFG_1` when the Board asks
//! for it, and repeats the idempotent housekeeping writes (I2C sleep and
//! watchdog off) the others also make.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_m5pm1::{Error as M5pm1Error, M5pm1};
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonSnapshot, Buttons},
};
use embedded_hal::i2c::I2c;

/// What a single click of the key does to the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SingleClickReset {
    /// Leave the M5PM1's setting as it is; factory units reset.
    Keep,
    /// Turn the reset off, so the click reaches the application.
    Disable,
}

/// Board-owned address and click behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct M5pm1PowerKeyConfig {
    address: u8,
    single_click_reset: SingleClickReset,
}

impl M5pm1PowerKeyConfig {
    /// Creates configuration for the M5PM1 at `address`.
    #[must_use]
    pub const fn new(address: u8, single_click_reset: SingleClickReset) -> Self {
        Self {
            address,
            single_click_reset,
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct M5pm1PowerKeyBindings<I2C> {
    i2c: I2C,
}

impl<I2C> M5pm1PowerKeyBindings<I2C> {
    /// Wraps this implementation's own view of the M5PM1's bus.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// Initialized power key.
pub struct M5pm1PowerKey<I2C> {
    i2c: I2C,
    address: u8,
}

/// Static factory used by generated Board composition.
pub struct M5pm1PowerKeyImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for M5pm1PowerKeyImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = M5pm1PowerKeyBindings<I2C>;
    type Config = M5pm1PowerKeyConfig;
    type Peripheral = M5pm1PowerKey<I2C>;
    type Error = M5pm1Error<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let mut i2c = bindings.i2c;
        {
            let mut pm1 = M5pm1::new(&mut i2c, config.address)?;
            pm1.disable_i2c_sleep()?;
            pm1.disable_watchdog()?;
            if config.single_click_reset == SingleClickReset::Disable {
                pm1.set_single_click_reset(false)?;
            }
            // A press recorded before start, such as the one that switched
            // the Board on, is not a press the application made.
            pm1.button_status()?;
        }
        Ok(M5pm1PowerKey {
            i2c,
            address: config.address,
        })
    }
}

impl<I2C: I2c> Buttons for M5pm1PowerKey<I2C> {
    type Error = M5pm1Error<I2C::Error>;

    /// Reports the key as button 0.
    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        let status = M5pm1::new(&mut self.i2c, self.address)?.button_status()?;
        Ok(ButtonSnapshot::new(&[
            status.pressed || status.pressed_since_last_read
        ]))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_peripheral::{PeripheralImplementation, buttons::Buttons};
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{
        M5pm1PowerKeyBindings, M5pm1PowerKeyConfig, M5pm1PowerKeyImplementation, SingleClickReset,
    };

    /// Register file at 6Eh whose `BTN_STATUS` press flag clears when read.
    struct Chip {
        registers: [u8; 0x50],
        writes: std::vec::Vec<(u8, u8)>,
    }

    impl Chip {
        fn new() -> Self {
            let mut registers = [0; 0x50];
            // Factory BTN_CFG_1, and the power-on press still recorded.
            registers[0x49] = 0x2a;
            registers[0x48] = 0x80;
            Self {
                registers,
                writes: std::vec::Vec::new(),
            }
        }
    }

    impl ErrorType for Chip {
        type Error = Infallible;
    }

    impl I2c for Chip {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x6e);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = *reg,
                    Operation::Write([reg, value]) => {
                        self.registers[usize::from(*reg)] = *value;
                        self.writes.push((*reg, *value));
                    }
                    Operation::Read([value]) => {
                        *value = self.registers[usize::from(register)];
                        if register == 0x48 {
                            self.registers[0x48] &= !0x80;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }

    #[test]
    fn follows_the_key_and_catches_a_press_between_reads() {
        let mut key = block_on(M5pm1PowerKeyImplementation::initialize(
            M5pm1PowerKeyBindings::new(Chip::new()),
            M5pm1PowerKeyConfig::new(0x6e, SingleClickReset::Keep),
        ))
        .expect("start");
        // Housekeeping only; the button configuration is left as it was.
        assert_eq!(key.i2c.writes, [(0x09, 0x00), (0x0a, 0x00)]);
        let snapshot = key.read().expect("read");
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot.is_pressed(0), Some(false));

        // Held down across reads.
        key.i2c.registers[0x48] = 0x81;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        key.i2c.registers[0x48] = 0x00;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(false));

        // Pressed and released between two reads.
        key.i2c.registers[0x48] = 0x80;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        assert_eq!(key.read().expect("read").is_pressed(0), Some(false));
    }

    #[test]
    fn disabling_the_click_reset_sets_only_bit_0() {
        let key = block_on(M5pm1PowerKeyImplementation::initialize(
            M5pm1PowerKeyBindings::new(Chip::new()),
            M5pm1PowerKeyConfig::new(0x6e, SingleClickReset::Disable),
        ))
        .expect("start");
        assert_eq!(key.i2c.registers[0x49], 0x2b);
        assert_eq!(key.i2c.writes, [(0x09, 0x00), (0x0a, 0x00), (0x49, 0x2b)]);
    }
}

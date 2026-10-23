//! The power key of an AXP192 or AXP2101 PMIC as one button.
//!
//! M5Stack's Core2, StickC, and Station wire their power key to the PWRON
//! pin of an AXP192, and the CoreS3 and Core2 v1.1 to that of an AXP2101,
//! both at `34h`; the chip ID tells them apart at start, as M5Unified
//! `Power_Class::begin` does. Neither PMIC reports the key's level. Each
//! latches a short press when the key is released before the long-press time
//! and a long press once it has been held that long, which M5Unified
//! `Power_Class::getKeyState` reads as a click (2) or a hold (1). This
//! implementation reports either as button 0 pressed in the one
//! [`Buttons::read`] that takes it, so a reader polling the snapshot sees a
//! press and a release for each.
//!
//! # Shared ownership
//!
//! The display, battery monitor, vibration, and audio implementations use the
//! same PMIC through their own views of the bus. This implementation sets
//! only the key's interrupt-enable bits (AXP192 `42h` bits 1:0, AXP2101 `41h`
//! bits 3:2) by read-modify-write, and clears only the key's status bits
//! (AXP192 `46h` bits 1:0, AXP2101 `49h` bits 3:2) by writing 1 to them, which
//! leaves every other interrupt's status as it was.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_axp192::{Axp192, Error as Axp192Error};
use barracuda_driver_axp2101::{Axp2101, Error as Axp2101Error};
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonSnapshot, Buttons},
};
use embedded_hal::i2c::I2c;

/// Board-owned PMIC address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxpPowerKeyConfig {
    address: u8,
}

impl AxpPowerKeyConfig {
    /// Creates configuration for the PMIC at `address`.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Which PMIC the key is wired to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pmic {
    /// X-Powers AXP192.
    Axp192,
    /// X-Powers AXP2101.
    Axp2101,
}

/// Register failure.
#[derive(Debug)]
pub enum AxpPowerKeyError<BusError> {
    /// No AXP192 or AXP2101 answered with its chip ID.
    UnsupportedPmic,
    /// An AXP192 register operation failed.
    Axp192(Axp192Error<BusError>),
    /// An AXP2101 register operation failed.
    Axp2101(Axp2101Error<BusError>),
}

/// Initialized power key.
pub struct AxpPowerKey<I2C> {
    i2c: I2C,
    address: u8,
    pmic: Pmic,
}

/// Static factory used by generated Board composition.
pub struct AxpPowerKeyImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for AxpPowerKeyImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = I2C;
    type Config = AxpPowerKeyConfig;
    type Peripheral = AxpPowerKey<I2C>;
    type Error = AxpPowerKeyError<I2C::Error>;

    async fn initialize(i2c: I2C, config: Self::Config) -> Result<Self::Peripheral, Self::Error> {
        let mut i2c = i2c;
        let address = config.address;
        // Both PMICs keep their ID in REG03H.
        let pmic = if Axp192::new(&mut i2c, address)
            .and_then(|mut pmic| pmic.is_present())
            .map_err(AxpPowerKeyError::Axp192)?
        {
            Pmic::Axp192
        } else if Axp2101::new(&mut i2c, address)
            .and_then(|mut pmic| pmic.is_present())
            .map_err(AxpPowerKeyError::Axp2101)?
        {
            Pmic::Axp2101
        } else {
            return Err(AxpPowerKeyError::UnsupportedPmic);
        };
        let mut key = AxpPowerKey { i2c, address, pmic };
        key.enable()?;
        // A press latched before start, such as the one that switched the
        // Board on, is not a press the application made.
        key.take_press()?;
        Ok(key)
    }
}

impl<I2C: I2c> AxpPowerKey<I2C> {
    /// Returns the PMIC that answered at initialization.
    #[must_use]
    pub const fn pmic(&self) -> Pmic {
        self.pmic
    }

    fn enable(&mut self) -> Result<(), AxpPowerKeyError<I2C::Error>> {
        match self.pmic {
            Pmic::Axp192 => Axp192::new(&mut self.i2c, self.address)
                .and_then(|mut pmic| pmic.enable_power_key_interrupts())
                .map_err(AxpPowerKeyError::Axp192),
            Pmic::Axp2101 => Axp2101::new(&mut self.i2c, self.address)
                .and_then(|mut pmic| pmic.enable_power_key_interrupts())
                .map_err(AxpPowerKeyError::Axp2101),
        }
    }

    /// Takes the latched short and long presses; true when there was either.
    fn take_press(&mut self) -> Result<bool, AxpPowerKeyError<I2C::Error>> {
        match self.pmic {
            Pmic::Axp192 => Axp192::new(&mut self.i2c, self.address)
                .and_then(|mut pmic| pmic.take_power_key_presses())
                .map(|presses| presses.any())
                .map_err(AxpPowerKeyError::Axp192),
            Pmic::Axp2101 => Axp2101::new(&mut self.i2c, self.address)
                .and_then(|mut pmic| pmic.take_power_key_presses())
                .map(|presses| presses.any())
                .map_err(AxpPowerKeyError::Axp2101),
        }
    }
}

impl<I2C: I2c> Buttons for AxpPowerKey<I2C> {
    type Error = AxpPowerKeyError<I2C::Error>;

    /// Reports the key as button 0, pressed when the PMIC latched a short or
    /// long press since the previous read.
    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        let pressed = self.take_press()?;
        Ok(ButtonSnapshot::new(&[pressed]))
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

    use super::{AxpPowerKeyConfig, AxpPowerKeyError, AxpPowerKeyImplementation, Pmic};

    /// Register file at 34h whose interrupt-status registers clear the bits
    /// written with 1, as both PMICs' do.
    struct Chip {
        registers: [u8; 0x100],
        status: u8,
    }

    impl Chip {
        fn axp192() -> Self {
            let mut registers = [0; 0x100];
            registers[0x03] = 0x03;
            // DC-DC under-voltage interrupts enabled (datasheet default 3Bh
            // without the key bits), a DC-DC1 under-voltage pending.
            registers[0x42] = 0b0011_1000;
            registers[0x46] = 0b0010_0000;
            Self {
                registers,
                status: 0x46,
            }
        }

        fn axp2101() -> Self {
            let mut registers = [0; 0x100];
            registers[0x03] = 0x4a;
            registers[0x41] = 0b1111_0000;
            // A VBUS insert pending.
            registers[0x49] = 0b1000_0000;
            Self {
                registers,
                status: 0x49,
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
            assert_eq!(address, 0x34);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = *reg,
                    Operation::Write([reg, value]) if *reg == self.status => {
                        self.registers[usize::from(*reg)] &= !*value;
                    }
                    Operation::Write([reg, value]) => self.registers[usize::from(*reg)] = *value,
                    Operation::Read([value]) => *value = self.registers[usize::from(register)],
                    _ => {}
                }
            }
            Ok(())
        }
    }

    #[test]
    fn axp192_reports_each_press_once_and_keeps_other_interrupts() {
        let mut chip = Chip::axp192();
        // The power-on press is still latched.
        chip.registers[0x46] |= 0b10;
        let mut key = block_on(AxpPowerKeyImplementation::initialize(
            chip,
            AxpPowerKeyConfig::new(0x34),
        ))
        .expect("start");
        assert_eq!(key.pmic(), Pmic::Axp192);
        assert_eq!(key.i2c.registers[0x42], 0b0011_1011);
        assert_eq!(key.i2c.registers[0x46], 0b0010_0000);
        let snapshot = key.read().expect("read");
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot.is_pressed(0), Some(false));

        // Short press (bit 1), then long press (bit 0).
        key.i2c.registers[0x46] |= 0b10;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        assert_eq!(key.read().expect("read").is_pressed(0), Some(false));
        key.i2c.registers[0x46] |= 0b01;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        assert_eq!(key.i2c.registers[0x46], 0b0010_0000);
    }

    #[test]
    fn axp2101_reads_bits_3_and_2_of_49h() {
        let mut key = block_on(AxpPowerKeyImplementation::initialize(
            Chip::axp2101(),
            AxpPowerKeyConfig::new(0x34),
        ))
        .expect("start");
        assert_eq!(key.pmic(), Pmic::Axp2101);
        assert_eq!(key.i2c.registers[0x41], 0b1111_1100);
        // A key edge (bit 1) is not a press.
        key.i2c.registers[0x49] |= 0b0000_0010;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(false));
        key.i2c.registers[0x49] |= 0b0000_1000;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        key.i2c.registers[0x49] |= 0b0000_0100;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        assert_eq!(key.read().expect("read").is_pressed(0), Some(false));
        assert_eq!(key.i2c.registers[0x49], 0b1000_0010);
    }

    #[test]
    fn another_chip_is_refused() {
        let mut chip = Chip::axp192();
        chip.registers[0x03] = 0x55;
        assert!(matches!(
            block_on(AxpPowerKeyImplementation::initialize(
                chip,
                AxpPowerKeyConfig::new(0x34),
            )),
            Err(AxpPowerKeyError::UnsupportedPmic)
        ));
    }
}

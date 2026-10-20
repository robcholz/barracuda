//! Low-level Driver for the M5Stack M5PM1 power-management controller.
//!
//! The M5PM1 is the microcontroller M5Stack's "Stamp Timer Power2" uses for
//! charging, power rails, and wake sources. Registers and bits follow
//! M5Unified's `M5PM1_Class` (src/utility/power/M5PM1_Class.inl, MIT
//! license). This driver covers the rail and housekeeping registers; the
//! charger, GPIO, and wake functions are left out until a consumer needs them.

#![no_std]

use embedded_hal::i2c::I2c;

/// Factory I2C address.
pub const DEFAULT_ADDRESS: u8 = 0x6E;

const POWER_CONFIG: u8 = 0x06;
const I2C_CONFIG: u8 = 0x09;
const WATCHDOG_COUNT: u8 = 0x0A;

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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{Error, M5pm1, Rail};

    struct Bus {
        registers: [u8; 0x10],
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
            if let [register, value] = bytes {
                self.registers[usize::from(*register)] = *value;
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
            registers: [0xff; 0x10],
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
            registers: [0; 0x10],
        };
        assert!(matches!(
            M5pm1::new(&mut bus, 0x80),
            Err(Error::InvalidAddress)
        ));
    }
}

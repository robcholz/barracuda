//! Low-level Driver for one X-Powers AXP2101 power-management IC.
//!
//! Register layout follows the AXP2101 datasheet (REG03H chip ID, REG90H LDO
//! on/off control, REG92H through REG97H ALDO/BLDO voltages, REG99H DLDO1
//! voltage), cross-checked against M5Stack's `M5Unified` `AXP2101_Class`.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// Fixed seven-bit I2C address of the AXP2101.
pub const ADDRESS: u8 = 0x34;

const CHIP_ID: u8 = 0x03;
const CHIP_ID_AXP2101: u8 = 0x4a;
const LDO_ENABLE: u8 = 0x90;
/// Voltage registers keep their setting in bits 4:0; bits 7:5 are reserved.
const VOLTAGE_FIELD: u8 = 0x1f;

/// Switchable, voltage-programmable AXP2101 LDO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ldo {
    /// ALDO1, 500 to 3500 mV in 100 mV steps.
    Aldo1,
    /// ALDO2, 500 to 3500 mV in 100 mV steps.
    Aldo2,
    /// ALDO3, 500 to 3500 mV in 100 mV steps.
    Aldo3,
    /// ALDO4, 500 to 3500 mV in 100 mV steps.
    Aldo4,
    /// BLDO1, 500 to 3500 mV in 100 mV steps.
    Bldo1,
    /// BLDO2, 500 to 3500 mV in 100 mV steps.
    Bldo2,
    /// DLDO1, 500 to 3300 mV in 100 mV steps.
    Dldo1,
}

impl Ldo {
    /// REG90H enable bit.
    const fn enable_mask(self) -> u8 {
        match self {
            Self::Aldo1 => 1 << 0,
            Self::Aldo2 => 1 << 1,
            Self::Aldo3 => 1 << 2,
            Self::Aldo4 => 1 << 3,
            Self::Bldo1 => 1 << 4,
            Self::Bldo2 => 1 << 5,
            Self::Dldo1 => 1 << 7,
        }
    }

    /// Voltage register and highest voltage; every listed LDO starts at
    /// 500 mV with 100 mV steps.
    const fn voltage_register(self) -> (u8, u16) {
        match self {
            Self::Aldo1 => (0x92, 3500),
            Self::Aldo2 => (0x93, 3500),
            Self::Aldo3 => (0x94, 3500),
            Self::Aldo4 => (0x95, 3500),
            Self::Bldo1 => (0x96, 3500),
            Self::Bldo2 => (0x97, 3500),
            Self::Dldo1 => (0x99, 3300),
        }
    }
}

/// AXP2101 register or configuration failure.
#[derive(Debug)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The voltage is outside the LDO's range or not on one of its steps.
    InvalidVoltage,
    /// An I2C register operation failed.
    Bus(BusError),
}

/// Borrowed access to one AXP2101 on a shared I2C bus.
pub struct Axp2101<'a, I2C> {
    i2c: &'a mut I2C,
    address: u8,
}

impl<'a, I2C> Axp2101<'a, I2C>
where
    I2C: I2c,
{
    /// Borrows one addressed AXP2101.
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

    /// Reads the chip ID register and reports whether it identifies an AXP2101.
    ///
    /// The AXP192 answers at the same address with a different ID, so this
    /// distinguishes the two.
    ///
    /// # Errors
    ///
    /// Returns an I2C error when no device acknowledges the read.
    pub fn is_present(&mut self) -> Result<bool, Error<I2C::Error>> {
        Ok(self.read(CHIP_ID)? == CHIP_ID_AXP2101)
    }

    /// Switches one LDO on or off without changing the others.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_enabled(&mut self, ldo: Ldo, enabled: bool) -> Result<(), Error<I2C::Error>> {
        let mask = ldo.enable_mask();
        self.update(LDO_ENABLE, |value| set_bits(value, mask, enabled))
    }

    /// Programs one LDO output voltage, leaving the reserved bits unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVoltage`] for a voltage the LDO cannot
    /// produce, or an I2C register error.
    pub fn set_millivolts(&mut self, ldo: Ldo, millivolts: u16) -> Result<(), Error<I2C::Error>> {
        let (register, maximum) = ldo.voltage_register();
        let code = voltage_code(millivolts, maximum).ok_or(Error::InvalidVoltage)?;
        self.update(register, |value| (value & !VOLTAGE_FIELD) | code)
    }

    /// Pulses an LDO used as an active-low reset line off and leaves it on.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn pulse_off<D: DelayNs>(
        &mut self,
        ldo: Ldo,
        off_ms: u32,
        settle_ms: u32,
        delay: &mut D,
    ) -> Result<(), Error<I2C::Error>> {
        self.set_enabled(ldo, false)?;
        delay.delay_ms(off_ms);
        self.set_enabled(ldo, true)?;
        delay.delay_ms(settle_ms);
        Ok(())
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

/// Converts a voltage into its 100 mV-step code above 500 mV.
fn voltage_code(millivolts: u16, maximum: u16) -> Option<u8> {
    if millivolts > maximum {
        return None;
    }
    let offset = millivolts.checked_sub(500)?;
    if offset.checked_rem(100)? != 0 {
        return None;
    }
    u8::try_from(offset.checked_div(100)?).ok()
}

const fn set_bits(value: u8, mask: u8, high: bool) -> u8 {
    if high { value | mask } else { value & !mask }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{ADDRESS, Axp2101, Error, LDO_ENABLE, Ldo};

    struct Bus {
        registers: [u8; 0x100],
        writes: std::vec::Vec<(u8, u8)>,
    }

    impl Default for Bus {
        fn default() -> Self {
            let mut registers = [0; 0x100];
            registers[0x03] = 0x4a;
            Self {
                registers,
                writes: std::vec::Vec::new(),
            }
        }
    }

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, ADDRESS);
            let mut register = None;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = Some(*reg),
                    Operation::Write([reg, value]) => {
                        self.registers[usize::from(*reg)] = *value;
                        self.writes.push((*reg, *value));
                    }
                    Operation::Read([value]) => {
                        *value = self.registers[usize::from(register.expect("register"))];
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay {
        elapsed_ms: u32,
    }

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.elapsed_ms += ns / 1_000_000;
        }
    }

    #[test]
    fn identifies_axp2101_and_rejects_axp192() {
        let mut bus = Bus::default();
        assert!(
            Axp2101::new(&mut bus, ADDRESS)
                .expect("valid address")
                .is_present()
                .expect("read")
        );
        bus.registers[0x03] = 0x03;
        assert!(
            !Axp2101::new(&mut bus, ADDRESS)
                .expect("valid address")
                .is_present()
                .expect("read")
        );
        assert!(matches!(
            Axp2101::new(&mut bus, 0xff),
            Err(Error::InvalidAddress)
        ));
    }

    #[test]
    fn enables_ldos_without_touching_others() {
        let mut bus = Bus::default();
        bus.registers[usize::from(LDO_ENABLE)] = 0x05;
        {
            let mut pmic = Axp2101::new(&mut bus, ADDRESS).expect("valid address");
            pmic.set_enabled(Ldo::Aldo4, true).expect("aldo4");
            pmic.set_enabled(Ldo::Dldo1, true).expect("dldo1");
            pmic.set_enabled(Ldo::Aldo3, false).expect("aldo3");
        }
        assert_eq!(bus.registers[usize::from(LDO_ENABLE)], 0x89);
    }

    #[test]
    fn programs_voltages_like_m5unified() {
        let mut bus = Bus::default();
        bus.registers[0x99] = 0xe0;
        {
            let mut pmic = Axp2101::new(&mut bus, ADDRESS).expect("valid address");
            // M5Unified CoreS3: ALDO4 3.3 V is `33 - 5` = 28.
            pmic.set_millivolts(Ldo::Aldo4, 3300).expect("aldo4");
            // M5GFX full brightness on BLDO1 and DLDO1: code 28.
            pmic.set_millivolts(Ldo::Bldo1, 3300).expect("bldo1");
            pmic.set_millivolts(Ldo::Dldo1, 2500).expect("dldo1");
        }
        assert_eq!(bus.registers[0x95], 28);
        assert_eq!(bus.registers[0x96], 28);
        // Reserved bits 7:5 are preserved.
        assert_eq!(bus.registers[0x99], 0xe0 | 20);
    }

    #[test]
    fn rejects_voltages_outside_range_or_step() {
        let mut bus = Bus::default();
        let mut pmic = Axp2101::new(&mut bus, ADDRESS).expect("valid address");
        assert!(matches!(
            pmic.set_millivolts(Ldo::Dldo1, 3400),
            Err(Error::InvalidVoltage)
        ));
        assert!(matches!(
            pmic.set_millivolts(Ldo::Aldo1, 400),
            Err(Error::InvalidVoltage)
        ));
        assert!(matches!(
            pmic.set_millivolts(Ldo::Bldo2, 1850),
            Err(Error::InvalidVoltage)
        ));
        assert!(pmic.set_millivolts(Ldo::Bldo2, 3500).is_ok());
    }

    #[test]
    fn pulses_ldo_reset_off_then_on() {
        let mut bus = Bus::default();
        bus.registers[usize::from(LDO_ENABLE)] = 0x0a;
        let mut delay = Delay::default();
        {
            let mut pmic = Axp2101::new(&mut bus, ADDRESS).expect("valid address");
            pmic.pulse_off(Ldo::Aldo2, 10, 120, &mut delay)
                .expect("pulse");
        }
        let enables: std::vec::Vec<u8> = bus
            .writes
            .iter()
            .filter(|(register, _)| *register == LDO_ENABLE)
            .map(|(_, value)| *value)
            .collect();
        assert_eq!(enables, [0x08, 0x0a]);
        assert_eq!(delay.elapsed_ms, 130);
    }
}

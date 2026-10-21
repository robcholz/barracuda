//! Low-level Driver for one X-Powers AXP192 power-management IC.
//!
//! Register layout follows the AXP192 datasheet (REG03H IC type, REG12H output
//! control, REG23H/26H/27H DC-DC voltages, REG28H LDO2/LDO3 voltages, and
//! REG95H/REG96H GPIO3/GPIO4 function and level), cross-checked against
//! M5Stack's `M5Unified` `AXP192_Class`.
//!
//! The registers fall into two groups with separate owners on a shared bus.
//! The regulator and GPIO registers above power and reset a Board's other
//! devices. The measurement registers belong to whoever reads the battery:
//! the status registers REG00H/REG01H, the ADC enable REG82H, and the ADC
//! results from REG78H (battery voltage, 12 bits at 1.1 mV) and REG7AH/7CH
//! (battery charge and discharge current, 13 bits at 0.5 mA). No method
//! touches registers of both groups.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// Fixed seven-bit I2C address of the AXP192.
pub const ADDRESS: u8 = 0x34;

const IC_TYPE: u8 = 0x03;
const IC_TYPE_AXP192: u8 = 0x03;
const OUTPUT_CONTROL: u8 = 0x12;
const DCDC2_VOLTAGE: u8 = 0x23;
const DCDC1_VOLTAGE: u8 = 0x26;
const DCDC3_VOLTAGE: u8 = 0x27;
const LDO23_VOLTAGE: u8 = 0x28;
const GPIO34_FUNCTION: u8 = 0x95;
const POWER_STATUS: u8 = 0x00;
const CHARGE_STATUS: u8 = 0x01;
const BATTERY_VOLTAGE: u8 = 0x78;
const BATTERY_CHARGE_CURRENT: u8 = 0x7a;
const BATTERY_DISCHARGE_CURRENT: u8 = 0x7c;
const ADC_ENABLE_1: u8 = 0x82;
/// REG82H bits 7 and 6: battery voltage and battery current ADCs.
const BATTERY_ADCS: u8 = 0b1100_0000;
const GPIO34_LEVEL: u8 = 0x96;

const EXTEN_BIT: u8 = 1 << 6;
/// REG95H bit 7 switches GPIO3/GPIO4 to their GPIO function; M5Unified sets it
/// together with the per-pin function bits.
const GPIO34_ENABLE: u8 = 1 << 7;

/// Switchable, voltage-programmable AXP192 output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rail {
    /// DC-DC1 buck converter, 700 to 3500 mV in 25 mV steps.
    Dcdc1,
    /// DC-DC2 buck converter, 700 to 2275 mV in 25 mV steps.
    Dcdc2,
    /// DC-DC3 buck converter, 700 to 3500 mV in 25 mV steps.
    Dcdc3,
    /// LDO2 regulator, 1800 to 3300 mV in 100 mV steps.
    Ldo2,
    /// LDO3 regulator, 1800 to 3300 mV in 100 mV steps.
    Ldo3,
}

impl Rail {
    const fn enable_mask(self) -> u8 {
        match self {
            Self::Dcdc1 => 1 << 0,
            Self::Dcdc3 => 1 << 1,
            Self::Ldo2 => 1 << 2,
            Self::Ldo3 => 1 << 3,
            Self::Dcdc2 => 1 << 4,
        }
    }

    /// Voltage register, field mask, field shift, minimum, maximum, and step.
    const fn voltage_field(self) -> (u8, u8, u8, u16, u16, u16) {
        match self {
            Self::Dcdc1 => (DCDC1_VOLTAGE, 0x7f, 0, 700, 3500, 25),
            Self::Dcdc2 => (DCDC2_VOLTAGE, 0x3f, 0, 700, 2275, 25),
            Self::Dcdc3 => (DCDC3_VOLTAGE, 0x7f, 0, 700, 3500, 25),
            Self::Ldo2 => (LDO23_VOLTAGE, 0xf0, 4, 1800, 3300, 100),
            Self::Ldo3 => (LDO23_VOLTAGE, 0x0f, 0, 1800, 3300, 100),
        }
    }
}

/// AXP192 GPIO with a level register in REG96H.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gpio {
    /// GPIO3, REG95H bits 1:0 and REG96H bit 0.
    Gpio3,
    /// GPIO4, REG95H bits 3:2 and REG96H bit 1.
    Gpio4,
}

impl Gpio {
    const fn level_mask(self) -> u8 {
        match self {
            Self::Gpio3 => 1 << 0,
            Self::Gpio4 => 1 << 1,
        }
    }

    /// REG95H function field mask and its value `01`, NMOS open-drain output.
    const fn open_drain_function(self) -> (u8, u8) {
        match self {
            Self::Gpio3 => (0b0000_0011, 0b0000_0001),
            Self::Gpio4 => (0b0000_1100, 0b0000_0100),
        }
    }
}

/// Battery state from REG00H and REG01H.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    /// REG00H bit 2: current flows into the battery.
    pub charging: bool,
    /// REG00H bit 5: VBUS is present.
    pub vbus_present: bool,
    /// REG01H bit 5: a battery is connected.
    pub battery_present: bool,
}

/// One battery measurement from the ADC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryMeasurement {
    /// Battery voltage in microvolts.
    pub microvolts: u32,
    /// Current into the battery in microamps.
    pub charge_microamps: u32,
    /// Current out of the battery in microamps.
    pub discharge_microamps: u32,
}

/// AXP192 register or configuration failure.
#[derive(Debug)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The voltage is outside the rail's range or not on one of its steps.
    InvalidVoltage,
    /// An I2C register operation failed.
    Bus(BusError),
}

/// Borrowed access to one AXP192 on a shared I2C bus.
pub struct Axp192<'a, I2C> {
    i2c: &'a mut I2C,
    address: u8,
}

impl<'a, I2C> Axp192<'a, I2C>
where
    I2C: I2c,
{
    /// Borrows one addressed AXP192.
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

    /// Reads the IC type register and reports whether it identifies an AXP192.
    ///
    /// The AXP2101 answers at the same address with a different IC type, so
    /// this distinguishes the two.
    ///
    /// # Errors
    ///
    /// Returns an I2C error when no device acknowledges the read.
    pub fn is_present(&mut self) -> Result<bool, Error<I2C::Error>> {
        Ok(self.read(IC_TYPE)? == IC_TYPE_AXP192)
    }

    /// Switches one output on or off without changing the others.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_enabled(&mut self, rail: Rail, enabled: bool) -> Result<(), Error<I2C::Error>> {
        let mask = rail.enable_mask();
        self.update(OUTPUT_CONTROL, |value| set_bits(value, mask, enabled))
    }

    /// Switches the EXTEN output on or off.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_exten_enabled(&mut self, enabled: bool) -> Result<(), Error<I2C::Error>> {
        self.update(OUTPUT_CONTROL, |value| set_bits(value, EXTEN_BIT, enabled))
    }

    /// Programs one output voltage, leaving the register's other bits unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVoltage`] for a voltage the rail cannot
    /// produce, or an I2C register error.
    pub fn set_millivolts(&mut self, rail: Rail, millivolts: u16) -> Result<(), Error<I2C::Error>> {
        let (register, mask, shift, minimum, maximum, step) = rail.voltage_field();
        let code = voltage_code(millivolts, minimum, maximum, step).ok_or(Error::InvalidVoltage)?;
        let field = code
            .checked_shl(u32::from(shift))
            .ok_or(Error::InvalidVoltage)?
            & mask;
        self.update(register, |value| (value & !mask) | field)
    }

    /// Configures GPIO3 or GPIO4 as an NMOS open-drain output and establishes
    /// its initial level. A high level releases the pin to its pull-up.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn configure_open_drain_output(
        &mut self,
        gpio: Gpio,
        initial_high: bool,
    ) -> Result<(), Error<I2C::Error>> {
        // Latch the level first so the pin does not glitch when its output
        // function is selected.
        self.set_level(gpio, initial_high)?;
        let (field_mask, function) = gpio.open_drain_function();
        self.update(GPIO34_FUNCTION, |value| {
            (value & !field_mask) | function | GPIO34_ENABLE
        })
    }

    /// Drives a GPIO configured as an output.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_level(&mut self, gpio: Gpio, high: bool) -> Result<(), Error<I2C::Error>> {
        let mask = gpio.level_mask();
        self.update(GPIO34_LEVEL, |value| set_bits(value, mask, high))
    }

    /// Pulses an active-low reset line on GPIO3 or GPIO4 and leaves it
    /// released high.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn pulse_reset_low<D: DelayNs>(
        &mut self,
        gpio: Gpio,
        low_ms: u32,
        settle_ms: u32,
        delay: &mut D,
    ) -> Result<(), Error<I2C::Error>> {
        self.configure_open_drain_output(gpio, false)?;
        delay.delay_ms(low_ms);
        self.set_level(gpio, true)?;
        delay.delay_ms(settle_ms);
        Ok(())
    }

    /// Switches on the battery voltage and current ADCs, leaving the other
    /// channels as they are.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn enable_battery_adc(&mut self) -> Result<(), Error<I2C::Error>> {
        self.update(ADC_ENABLE_1, |value| value | BATTERY_ADCS)
    }

    /// Reads the charge, supply, and battery-presence flags.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn battery_status(&mut self) -> Result<BatteryStatus, Error<I2C::Error>> {
        let power = self.read(POWER_STATUS)?;
        let charge = self.read(CHARGE_STATUS)?;
        Ok(BatteryStatus {
            charging: power & (1 << 2) != 0,
            vbus_present: power & (1 << 5) != 0,
            battery_present: charge & (1 << 5) != 0,
        })
    }

    /// Reads the battery voltage and both battery currents.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn battery_measurement(&mut self) -> Result<BatteryMeasurement, Error<I2C::Error>> {
        let voltage = self.read_pair(BATTERY_VOLTAGE)?;
        let charge = self.read_pair(BATTERY_CHARGE_CURRENT)?;
        let discharge = self.read_pair(BATTERY_DISCHARGE_CURRENT)?;
        // 12-bit voltage: high byte then the low nibble; 13-bit currents:
        // high byte then the low five bits.
        let voltage = (u32::from(voltage[0]) << 4) | u32::from(voltage[1] & 0x0f);
        let charge = (u32::from(charge[0]) << 5) | u32::from(charge[1] & 0x1f);
        let discharge = (u32::from(discharge[0]) << 5) | u32::from(discharge[1] & 0x1f);
        Ok(BatteryMeasurement {
            microvolts: voltage.saturating_mul(1_100),
            charge_microamps: charge.saturating_mul(500),
            discharge_microamps: discharge.saturating_mul(500),
        })
    }

    fn read_pair(&mut self, register: u8) -> Result<[u8; 2], Error<I2C::Error>> {
        let mut value = [0; 2];
        self.i2c
            .write_read(self.address, &[register], &mut value)
            .map_err(Error::Bus)?;
        Ok(value)
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

/// Converts a voltage into its register code: `(millivolts - minimum) / step`.
fn voltage_code(millivolts: u16, minimum: u16, maximum: u16, step: u16) -> Option<u8> {
    if millivolts > maximum {
        return None;
    }
    let offset = millivolts.checked_sub(minimum)?;
    if offset.checked_rem(step)? != 0 {
        return None;
    }
    u8::try_from(offset.checked_div(step)?).ok()
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

    use super::{
        ADDRESS, Axp192, BatteryMeasurement, BatteryStatus, DCDC3_VOLTAGE, Error, GPIO34_FUNCTION,
        GPIO34_LEVEL, Gpio, LDO23_VOLTAGE, OUTPUT_CONTROL, Rail,
    };

    struct Bus {
        registers: [u8; 0x100],
        writes: std::vec::Vec<(u8, u8)>,
    }

    impl Default for Bus {
        fn default() -> Self {
            let mut registers = [0; 0x100];
            registers[0x03] = 0x03;
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
                    Operation::Read(values) => {
                        let start = usize::from(register.expect("register"));
                        for (offset, value) in values.iter_mut().enumerate() {
                            *value = self.registers[start + offset];
                        }
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
    fn identifies_axp192_and_rejects_other_ic_types() {
        let mut bus = Bus::default();
        assert!(
            Axp192::new(&mut bus, ADDRESS)
                .expect("valid address")
                .is_present()
                .expect("read")
        );
        bus.registers[0x03] = 0x4a;
        assert!(
            !Axp192::new(&mut bus, ADDRESS)
                .expect("valid address")
                .is_present()
                .expect("read")
        );
        assert!(matches!(
            Axp192::new(&mut bus, 0x80),
            Err(Error::InvalidAddress)
        ));
    }

    #[test]
    fn enables_rails_without_touching_other_outputs() {
        let mut bus = Bus::default();
        bus.registers[usize::from(OUTPUT_CONTROL)] = 0x41;
        {
            let mut pmic = Axp192::new(&mut bus, ADDRESS).expect("valid address");
            pmic.set_enabled(Rail::Ldo2, true).expect("ldo2 on");
            pmic.set_enabled(Rail::Dcdc3, true).expect("dcdc3 on");
            pmic.set_enabled(Rail::Dcdc1, false).expect("dcdc1 off");
        }
        assert_eq!(bus.registers[usize::from(OUTPUT_CONTROL)], 0x46);
    }

    #[test]
    fn programs_ldo_nibbles_independently() {
        let mut bus = Bus::default();
        bus.registers[usize::from(LDO23_VOLTAGE)] = 0x0c;
        {
            let mut pmic = Axp192::new(&mut bus, ADDRESS).expect("valid address");
            // M5Unified: LDO2 3300 mV is code 15 in the high nibble.
            pmic.set_millivolts(Rail::Ldo2, 3300).expect("ldo2");
        }
        assert_eq!(bus.registers[usize::from(LDO23_VOLTAGE)], 0xfc);
        {
            let mut pmic = Axp192::new(&mut bus, ADDRESS).expect("valid address");
            pmic.set_millivolts(Rail::Ldo3, 2500).expect("ldo3");
        }
        assert_eq!(bus.registers[usize::from(LDO23_VOLTAGE)], 0xf7);
    }

    #[test]
    fn programs_dcdc3_and_keeps_reserved_bit() {
        let mut bus = Bus::default();
        bus.registers[usize::from(DCDC3_VOLTAGE)] = 0x80;
        {
            let mut pmic = Axp192::new(&mut bus, ADDRESS).expect("valid address");
            // M5GFX Core2 backlight at full brightness: code 103 = 3275 mV.
            pmic.set_millivolts(Rail::Dcdc3, 3275).expect("dcdc3");
        }
        assert_eq!(bus.registers[usize::from(DCDC3_VOLTAGE)], 0x80 | 103);
    }

    #[test]
    fn rejects_voltages_outside_range_or_step() {
        let mut bus = Bus::default();
        let mut pmic = Axp192::new(&mut bus, ADDRESS).expect("valid address");
        assert!(matches!(
            pmic.set_millivolts(Rail::Ldo2, 3400),
            Err(Error::InvalidVoltage)
        ));
        assert!(matches!(
            pmic.set_millivolts(Rail::Ldo3, 1700),
            Err(Error::InvalidVoltage)
        ));
        assert!(matches!(
            pmic.set_millivolts(Rail::Dcdc3, 3010),
            Err(Error::InvalidVoltage)
        ));
        assert!(matches!(
            pmic.set_millivolts(Rail::Dcdc2, 2300),
            Err(Error::InvalidVoltage)
        ));
    }

    #[test]
    fn pulses_gpio4_reset_as_open_drain_output() {
        let mut bus = Bus::default();
        bus.registers[usize::from(GPIO34_FUNCTION)] = 0x02;
        bus.registers[usize::from(GPIO34_LEVEL)] = 0x03;
        let mut delay = Delay::default();
        {
            let mut pmic = Axp192::new(&mut bus, ADDRESS).expect("valid address");
            pmic.pulse_reset_low(Gpio::Gpio4, 10, 120, &mut delay)
                .expect("pulse");
        }
        // GPIO3 bits stay as they were; GPIO4 becomes an open-drain output.
        assert_eq!(bus.registers[usize::from(GPIO34_FUNCTION)], 0x86);
        assert_eq!(bus.registers[usize::from(GPIO34_LEVEL)], 0x03);
        assert_eq!(delay.elapsed_ms, 130);
        let levels: std::vec::Vec<u8> = bus
            .writes
            .iter()
            .filter(|(register, _)| *register == GPIO34_LEVEL)
            .map(|(_, value)| *value)
            .collect();
        assert_eq!(levels, [0x01, 0x03]);
    }

    #[test]
    fn reads_the_battery_through_the_measurement_registers_only() {
        let mut bus = Bus::default();
        bus.registers[0x82] = 0x03;
        bus.registers[0x00] = 0b0010_0100;
        bus.registers[0x01] = 0b0010_0000;
        // 3.8016 V = 3456 x 1.1 mV; 120 mA charge = 240 x 0.5 mA.
        bus.registers[0x78] = 0xd8;
        bus.registers[0x79] = 0x00;
        bus.registers[0x7a] = 0x07;
        bus.registers[0x7b] = 0x10;
        {
            let mut pmic = Axp192::new(&mut bus, ADDRESS).expect("valid address");
            pmic.enable_battery_adc().expect("adc");
            assert_eq!(
                pmic.battery_status().expect("status"),
                BatteryStatus {
                    charging: true,
                    vbus_present: true,
                    battery_present: true,
                }
            );
            assert_eq!(
                pmic.battery_measurement().expect("measurement"),
                BatteryMeasurement {
                    microvolts: 3_801_600,
                    charge_microamps: 120_000,
                    discharge_microamps: 0,
                }
            );
        }
        assert_eq!(bus.registers[0x82], 0xc3);
        assert_eq!(bus.writes, [(0x82, 0xc3)]);
    }
}

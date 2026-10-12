//! Texas Instruments INA226 current, voltage, and power monitor.
//!
//! Source: TI datasheet SBOS547C, "INA226 36V, 16-Bit, Ultra-Precise I2C
//! Output Current, Voltage, and Power Monitor With Alert", June 2011, revised
//! August 2026. Section and table numbers below refer to that revision.
//!
//! Modelled: the register set and power-on values (Table 7-1), register
//! pointer writes and two-byte MSB-first register access (6.5.5.3), the
//! configuration register reset bit (Table 7-2), the conversion sequence and
//! its timing (6.3.1, Tables 7-3 to 7-6, tCT in 5.5), the Current and Power
//! equations (6.5.1, Equations 3 and 4), and the Conversion Ready flag
//! (7.1.7). Not modelled: the Alert pin functions and limits, high-speed mode,
//! and the SMBus timeout.
//!
//! Backdoor layout: register `r` occupies bytes `2r` (MSB) and `2r + 1`
//! (LSB). Poking the Shunt Voltage (01h) or Bus Voltage (02h) register sets
//! the analog input in register units; the register shows it once a
//! conversion completes. Other registers are poked directly.

use std::time::Duration;

use crate::device::{DataNack, DeviceContext, I2cDevice, RegisterRangeError};

const CONFIGURATION: u8 = 0x00;
const SHUNT_VOLTAGE: u8 = 0x01;
const BUS_VOLTAGE: u8 = 0x02;
const POWER: u8 = 0x03;
const CURRENT: u8 = 0x04;
const CALIBRATION: u8 = 0x05;
const MASK_ENABLE: u8 = 0x06;
const ALERT_LIMIT: u8 = 0x07;
const MANUFACTURER_ID: u8 = 0xfe;
const DIE_ID: u8 = 0xff;

/// Table 7-1: power-on reset value of the Configuration Register.
const CONFIGURATION_POR: u16 = 0x4127;
/// Table 7-1: Manufacturer ID Register value.
const MANUFACTURER: u16 = 0x5449;
/// Table 7-1: Die ID Register value (2260h; 2261h on some dies, note 3).
const DIE: u16 = 0x2260;
/// Table 7-2: RST, bit 15 of the Configuration Register.
const RESET_BIT: u16 = 1 << 15;
/// 7.1.7: CVRF, bit 3 of the Mask/Enable Register.
const CONVERSION_READY: u16 = 1 << 3;

/// Table 7-3: number of averages for AVG[11:9].
const AVERAGES: [u32; 8] = [1, 4, 16, 64, 128, 256, 512, 1024];
/// 5.5 Electrical Characteristics, tCT maximum for CT = 000..111, in
/// microseconds (the model completes a conversion only after the maximum).
const CONVERSION_MAX_US: [u64; 8] = [154, 224, 365, 646, 1_210, 2_328, 4_572, 9_068];

const BACKDOOR_BYTES: usize = 512;

/// Rule identifiers reported by this model.
pub mod rules {
    /// 6.5.5.3: a register write carries the pointer and exactly two bytes.
    pub const WRITE_LENGTH: &str = "INA226 SBOS547C 6.5.5.3 register write length";
    /// 6.5.5.3: a register read returns the MSB then the LSB.
    pub const READ_LENGTH: &str = "INA226 SBOS547C 6.5.5.3 register read length";
    /// Table 7-1: the pointer names a register of the register set.
    pub const UNKNOWN_REGISTER: &str = "INA226 SBOS547C Table 7-1 register address";
    /// Table 7-1: type R registers are read-only.
    pub const READ_ONLY: &str = "INA226 SBOS547C Table 7-1 read-only register";
}

/// Behavioural model of one INA226.
#[derive(Clone, Debug)]
pub struct Ina226Model {
    pointer: u8,
    configuration: u16,
    shunt: u16,
    bus: u16,
    power: u16,
    current: u16,
    calibration: u16,
    mask_enable: u16,
    alert_limit: u16,
    shunt_input: u16,
    bus_input: u16,
    /// End of the conversion in progress, if any.
    conversion_end: Option<Duration>,
}

impl Ina226Model {
    /// Creates a device in its power-on state with zero analog inputs.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pointer: 0,
            configuration: CONFIGURATION_POR,
            shunt: 0,
            bus: 0,
            power: 0,
            current: 0,
            calibration: 0,
            mask_enable: 0,
            alert_limit: 0,
            shunt_input: 0,
            bus_input: 0,
            conversion_end: None,
        }
    }

    /// Sets the analog inputs in register units: shunt LSB 2.5 µV (two's
    /// complement), bus LSB 1.25 mV (5.5, 7.1.2, 7.1.3).
    #[must_use]
    pub const fn with_inputs(mut self, shunt: i16, bus: u16) -> Self {
        self.shunt_input = shunt as u16;
        self.bus_input = bus & 0x7fff;
        self
    }

    fn reset(&mut self, now: Duration) {
        let (shunt_input, bus_input) = (self.shunt_input, self.bus_input);
        *self = Self::new();
        self.shunt_input = shunt_input;
        self.bus_input = bus_input;
        self.start_conversion(now);
    }

    /// 6.3.1 and Table 7-6: the time one complete conversion takes in the
    /// configured mode, or `None` in power-down.
    fn conversion_time(&self) -> Option<Duration> {
        let mode = self.configuration & 0b111;
        let averages = u64::from(AVERAGES[usize::from((self.configuration >> 9) & 0b111)]);
        let bus = CONVERSION_MAX_US[usize::from((self.configuration >> 6) & 0b111)];
        let shunt = CONVERSION_MAX_US[usize::from((self.configuration >> 3) & 0b111)];
        let per_sample = match mode & 0b011 {
            0b000 => return None,
            0b001 => shunt,
            0b010 => bus,
            _ => shunt + bus,
        };
        Some(Duration::from_micros(per_sample * averages))
    }

    const fn continuous(&self) -> bool {
        self.configuration & 0b100 != 0
    }

    /// Table 7-2: writing the Configuration Register starts a new conversion.
    fn start_conversion(&mut self, now: Duration) {
        self.conversion_end = self.conversion_time().map(|time| now + time);
    }

    /// Latches results whose conversion has completed by `now`.
    fn update(&mut self, now: Duration) {
        let Some(end) = self.conversion_end else {
            return;
        };
        if now < end {
            return;
        }
        let mode = self.configuration & 0b011;
        if mode & 0b001 != 0 {
            self.shunt = self.shunt_input;
        }
        if mode & 0b010 != 0 {
            self.bus = self.bus_input;
        }
        // 6.5.1, Equation 3: Current = ShuntVoltage × Calibration / 2048.
        let current = i32::from(self.shunt as i16) * i32::from(self.calibration) / 2048;
        self.current = current as u16;
        // 6.5.1, Equation 4: Power = Current × BusVoltage / 20000. The
        // datasheet does not state the sign handling of a negative current;
        // the model uses the current's magnitude.
        let power = u64::from(current.unsigned_abs()) * u64::from(self.bus) / 20_000;
        self.power = u16::try_from(power).unwrap_or(u16::MAX);
        // 7.1.7: CVRF is set after all conversions and calculations.
        self.mask_enable |= CONVERSION_READY;
        // Continuous modes keep converting; the model then tracks the inputs
        // from one completed conversion period to the next.
        self.conversion_end = if self.continuous() {
            self.conversion_time().map(|time| {
                let periods = (now - end).as_nanos() / time.as_nanos().max(1);
                end + time * u32::try_from(periods + 1).unwrap_or(u32::MAX)
            })
        } else {
            None
        };
    }

    fn register(&self, pointer: u8) -> Option<u16> {
        Some(match pointer {
            CONFIGURATION => self.configuration,
            SHUNT_VOLTAGE => self.shunt,
            BUS_VOLTAGE => self.bus,
            POWER => self.power,
            CURRENT => self.current,
            CALIBRATION => self.calibration,
            MASK_ENABLE => self.mask_enable,
            ALERT_LIMIT => self.alert_limit,
            MANUFACTURER_ID => MANUFACTURER,
            DIE_ID => DIE,
            _ => return None,
        })
    }

    fn write_register(&mut self, context: &mut DeviceContext<'_>, pointer: u8, value: u16) {
        match pointer {
            CONFIGURATION if value & RESET_BIT != 0 => self.reset(context.now()),
            CONFIGURATION => {
                self.configuration = value;
                // 7.1.7: a configuration write clears CVRF, except when it
                // selects power-down.
                if value & 0b011 != 0 {
                    self.mask_enable &= !CONVERSION_READY;
                }
                self.start_conversion(context.now());
            }
            CALIBRATION => self.calibration = value,
            MASK_ENABLE => {
                self.mask_enable =
                    (value & !CONVERSION_READY) | (self.mask_enable & CONVERSION_READY);
            }
            ALERT_LIMIT => self.alert_limit = value,
            SHUNT_VOLTAGE | BUS_VOLTAGE | POWER | CURRENT | MANUFACTURER_ID | DIE_ID => {
                context.violation(
                    rules::READ_ONLY,
                    format!("write of {value:#06x} to read-only register {pointer:#04x}"),
                );
            }
            _ => context.violation(
                rules::UNKNOWN_REGISTER,
                format!("write to undefined register {pointer:#04x}"),
            ),
        }
    }
}

impl Default for Ina226Model {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Ina226Model {
    fn model(&self) -> &'static str {
        "ina226"
    }

    fn attached(&mut self, context: &mut DeviceContext<'_>) {
        // Table 7-6: the power-on mode converts shunt and bus continuously.
        self.start_conversion(context.now());
    }

    fn write(&mut self, context: &mut DeviceContext<'_>, bytes: &[u8]) -> Result<(), DataNack> {
        self.update(context.now());
        let Some((&pointer, data)) = bytes.split_first() else {
            return Ok(());
        };
        if self.register(pointer).is_none() {
            context.violation(
                rules::UNKNOWN_REGISTER,
                format!("register pointer {pointer:#04x} is not in the register set"),
            );
        }
        self.pointer = pointer;
        match data {
            [] => {}
            [msb, lsb] => self.write_register(context, pointer, u16::from_be_bytes([*msb, *lsb])),
            _ => context.violation(
                rules::WRITE_LENGTH,
                format!(
                    "write to register {pointer:#04x} carried {} data bytes instead of 2",
                    data.len()
                ),
            ),
        }
        Ok(())
    }

    fn read(&mut self, context: &mut DeviceContext<'_>, buffer: &mut [u8]) {
        self.update(context.now());
        let value = self.register(self.pointer).unwrap_or(0);
        if buffer.len() > 2 {
            context.violation(
                rules::READ_LENGTH,
                format!(
                    "read of {} bytes from register {:#04x}; a register is 2 bytes",
                    buffer.len(),
                    self.pointer
                ),
            );
        }
        // The model returns FFh past the register, as an idle bus would.
        let bytes = value.to_be_bytes();
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = bytes.get(index).copied().unwrap_or(0xff);
        }
        // 7.1.7: reading the Mask/Enable Register clears CVRF.
        if self.pointer == MASK_ENABLE {
            self.mask_enable &= !CONVERSION_READY;
        }
    }

    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
        let mut bytes = [0; BACKDOOR_BYTES];
        for pointer in 0..=u8::MAX {
            if let Some(value) = self.register(pointer) {
                let start = usize::from(pointer) * 2;
                bytes[start..start + 2].copy_from_slice(&value.to_be_bytes());
            }
        }
        let source = crate::device::register_range(&bytes, offset, buffer.len())?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn poke(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RegisterRangeError> {
        let length = bytes.len();
        let error = RegisterRangeError { offset, length };
        if !offset.is_multiple_of(2)
            || !length.is_multiple_of(2)
            || offset + length > BACKDOOR_BYTES
        {
            return Err(error);
        }
        for (index, word) in bytes.as_chunks::<2>().0.iter().enumerate() {
            let pointer = u8::try_from(offset / 2 + index).map_err(|_| error)?;
            let value = u16::from_be_bytes([word[0], word[1]]);
            match pointer {
                CONFIGURATION => self.configuration = value,
                SHUNT_VOLTAGE => self.shunt_input = value,
                BUS_VOLTAGE => self.bus_input = value & 0x7fff,
                POWER => self.power = value,
                CURRENT => self.current = value,
                CALIBRATION => self.calibration = value,
                MASK_ENABLE => self.mask_enable = value,
                ALERT_LIMIT => self.alert_limit = value,
                _ => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::i2c::I2c as _;

    use super::*;
    use crate::{Clock, VirtualHardware};

    const ADDRESS: u8 = 0x40;

    fn setup(model: Ina226Model) -> (VirtualHardware, crate::VirtualI2cBus) {
        let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
        hardware
            .attach("I2C0", u64::from(ADDRESS), Box::new(model))
            .expect("attach");
        let bus = hardware.i2c_bus("I2C0").expect("bus");
        (hardware, bus)
    }

    fn read(bus: &mut crate::VirtualI2cBus, register: u8) -> u16 {
        let mut value = [0; 2];
        bus.write_read(ADDRESS, &[register], &mut value)
            .expect("read");
        u16::from_be_bytes(value)
    }

    #[test]
    fn identity_and_power_on_values_follow_table_7_1() {
        let (hardware, mut bus) = setup(Ina226Model::new());
        assert_eq!(read(&mut bus, MANUFACTURER_ID), 0x5449);
        assert_eq!(read(&mut bus, DIE_ID), 0x2260);
        assert_eq!(read(&mut bus, CONFIGURATION), 0x4127);
        assert!(hardware.violations().is_empty());
    }

    #[test]
    fn measurements_appear_after_the_default_conversion_completes() {
        let (hardware, mut bus) = setup(Ina226Model::new().with_inputs(8_000, 9_584));
        assert_eq!(
            read(&mut bus, BUS_VOLTAGE),
            0,
            "power-on value until converted"
        );
        // Default: 1 average of 1.1 ms shunt + 1.1 ms bus, at most 2 × 1.21 ms.
        hardware.clock().advance(Duration::from_micros(2_419));
        assert_eq!(read(&mut bus, BUS_VOLTAGE), 0);
        hardware.clock().advance(Duration::from_micros(1));
        assert_eq!(read(&mut bus, BUS_VOLTAGE), 9_584);
        assert_eq!(read(&mut bus, SHUNT_VOLTAGE), 8_000);
        assert_eq!(
            read(&mut bus, MASK_ENABLE) & CONVERSION_READY,
            CONVERSION_READY
        );
        assert_eq!(
            read(&mut bus, MASK_ENABLE) & CONVERSION_READY,
            0,
            "a read clears CVRF"
        );
    }

    #[test]
    fn current_and_power_follow_equations_3_and_4() {
        // The 6.5.1 example: 20 mV across 2 mΩ, 11.98 V, Calibration A00h.
        let (hardware, mut bus) = setup(Ina226Model::new().with_inputs(8_000, 9_584));
        bus.write(ADDRESS, &[CALIBRATION, 0x0a, 0x00])
            .expect("calibrate");
        hardware.clock().advance(Duration::from_millis(3));
        assert_eq!(read(&mut bus, CURRENT), 10_000);
        assert_eq!(read(&mut bus, POWER), 4_792);
    }

    #[test]
    fn a_configuration_write_restarts_conversion_and_reset_restores_defaults() {
        let (hardware, mut bus) = setup(Ina226Model::new().with_inputs(0, 100));
        // Bus voltage only, triggered, 8.244 ms conversions, 4 averages.
        bus.write(ADDRESS, &[CONFIGURATION, 0x43, 0xc2])
            .expect("configure");
        hardware
            .clock()
            .advance(Duration::from_micros(4 * 9_068 - 1));
        assert_eq!(read(&mut bus, BUS_VOLTAGE), 0);
        hardware.clock().advance(Duration::from_micros(1));
        assert_eq!(read(&mut bus, BUS_VOLTAGE), 100);
        bus.write(ADDRESS, &[CONFIGURATION, 0x80, 0x00])
            .expect("reset");
        assert_eq!(read(&mut bus, CONFIGURATION), 0x4127);
        assert_eq!(read(&mut bus, BUS_VOLTAGE), 0);
        assert!(hardware.violations().is_empty());
    }

    #[test]
    fn protocol_misuse_is_reported() {
        let (hardware, mut bus) = setup(Ina226Model::new());
        bus.write(ADDRESS, &[BUS_VOLTAGE, 0, 1]).expect("write");
        bus.write(ADDRESS, &[CALIBRATION, 1]).expect("write");
        bus.write(ADDRESS, &[0x42]).expect("write");
        let mut long = [0; 3];
        bus.write_read(ADDRESS, &[CONFIGURATION], &mut long)
            .expect("read");
        let rules: Vec<_> = hardware
            .violations()
            .into_iter()
            .map(|violation| violation.violation.rule)
            .collect();
        assert_eq!(
            rules,
            [
                rules::READ_ONLY,
                rules::WRITE_LENGTH,
                rules::UNKNOWN_REGISTER,
                rules::READ_LENGTH
            ]
        );
    }

    #[test]
    fn the_backdoor_sets_inputs_and_reads_registers() {
        let (hardware, mut bus) = setup(Ina226Model::new());
        hardware
            .write_registers("I2C0", u64::from(ADDRESS), 4, &[0x25, 0x70])
            .expect("poke bus input");
        hardware.clock().advance(Duration::from_millis(3));
        assert_eq!(read(&mut bus, BUS_VOLTAGE), 0x2570);
        assert_eq!(
            hardware
                .read_registers("I2C0", u64::from(ADDRESS), 0x1fc, 4)
                .expect("peek"),
            [0x54, 0x49, 0x22, 0x60]
        );
    }
}

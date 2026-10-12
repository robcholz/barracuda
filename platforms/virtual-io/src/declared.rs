//! Device models behind Board-declared peripherals.
//!
//! Generated Board composition announces every peripheral the Board declares
//! before it initializes any of them (see [`crate::hal::declare_peripheral`]).
//! This module maps the peripheral's implementation identifier to the chip
//! model its driver expects and attaches that model at the declared bus and
//! address, so the System runs the real chip driver and peripheral
//! implementation against the model.

use crate::{
    device::I2cDevice,
    hardware::{HardwareError, VirtualHardware},
    models::{Bq27220Model, Ina226Model, Rx8130ceModel},
};

/// Bus voltage of the modelled INA226 supply: 5.000 V in 1.25 mV LSBs
/// (SBOS547C 7.1.3).
const INA226_BUS_LSB: u16 = 4_000;
/// Load current through the modelled INA226 shunt, in milliamperes.
const INA226_LOAD_MILLIAMPS: i64 = 200;
/// Shunt resistance the INA226 implementation assumes without a parameter.
const INA226_DEFAULT_SHUNT_MICRO_OHMS: i64 = 5_000;

/// One peripheral as the Board declared it.
#[derive(Clone, Copy, Debug)]
pub struct PeripheralDeclaration {
    /// Board peripheral name, such as `real-time-clock`.
    pub name: &'static str,
    /// Peripheral implementation identifier, such as `rx8130ce-rtc`.
    pub implementation: &'static str,
    /// Binding roles and the chip-native resource each resolves to: the
    /// controller of an I2C device, the pin of a digital binding.
    pub bindings: &'static [(&'static str, &'static str)],
    /// Scalar parameters with defaults applied, as their YAML text.
    pub parameters: &'static [(&'static str, &'static str)],
}

impl PeripheralDeclaration {
    fn binding(&self, role: &str) -> Option<&'static str> {
        self.bindings
            .iter()
            .find_map(|(name, resource)| (*name == role).then_some(*resource))
    }

    fn parameter(&self, name: &str) -> Option<&'static str> {
        self.parameters
            .iter()
            .find_map(|(key, value)| (*key == name).then_some(*value))
    }

    fn integer(&self, name: &str) -> Result<Option<i64>, DeclarationError> {
        self.parameter(name)
            .map(|value| {
                value
                    .parse()
                    .map_err(|_error| DeclarationError::InvalidParameter {
                        parameter: String::from(name),
                        value: String::from(value),
                    })
            })
            .transpose()
    }
}

/// A device model attached for a declared peripheral.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredDevice {
    /// Controller the model answers on.
    pub bus: &'static str,
    /// Seven-bit address.
    pub address: u8,
    /// Device model name.
    pub model: &'static str,
}

/// A declared peripheral's model could not be attached.
#[derive(Debug, thiserror::Error)]
pub enum DeclarationError {
    /// The declaration lacks the binding the model sits on.
    #[error("the declaration has no `{0}` binding")]
    MissingBinding(&'static str),
    /// The declaration lacks the device address.
    #[error("the declaration has no `address` parameter")]
    MissingAddress,
    /// A parameter is not the integer the model needs.
    #[error("parameter `{parameter}` = {value:?} is not an integer")]
    InvalidParameter {
        /// Parameter name.
        parameter: String,
        /// Declared text.
        value: String,
    },
    /// The model could not be attached.
    #[error(transparent)]
    Hardware(#[from] HardwareError),
}

/// Chip models for the peripheral implementations this crate can back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    Rx8130ce,
    Ina226,
    Bq27220,
}

impl Model {
    fn for_implementation(implementation: &str) -> Option<Self> {
        match implementation {
            "rx8130ce-rtc" => Some(Self::Rx8130ce),
            "ina226-power-monitor" => Some(Self::Ina226),
            "bq27220-power-monitor" => Some(Self::Bq27220),
            _ => None,
        }
    }

    fn device(
        self,
        declaration: &PeripheralDeclaration,
        unix_seconds: u64,
    ) -> Result<Box<dyn I2cDevice>, DeclarationError> {
        Ok(match self {
            // The Board's clock kept time on its backup supply; outside the
            // chip's calendar it starts from its initial power-on state.
            Self::Rx8130ce => {
                Box::new(Rx8130ceModel::keeping_utc(unix_seconds).unwrap_or_default())
            }
            Self::Ina226 => {
                let shunt_micro_ohms = declaration
                    .integer("shunt-micro-ohms")?
                    .unwrap_or(INA226_DEFAULT_SHUNT_MICRO_OHMS);
                // Shunt voltage in 2.5 µV LSBs (7.1.2): I × R / 2.5 µV.
                let shunt = (INA226_LOAD_MILLIAMPS * shunt_micro_ohms / 2_500)
                    .clamp(i64::from(i16::MIN), i64::from(i16::MAX));
                let shunt = i16::try_from(shunt).unwrap_or(i16::MAX);
                Box::new(Ina226Model::new().with_inputs(shunt, INA226_BUS_LSB))
            }
            Self::Bq27220 => Box::new(Bq27220Model::new()),
        })
    }
}

/// Attaches the device model behind one declared peripheral.
///
/// Returns `Ok(None)` when no model backs the implementation.
///
/// # Errors
///
/// Returns [`DeclarationError`] when the declaration lacks the bus or
/// address the model needs, or the model cannot be attached there.
pub fn attach(
    hardware: &VirtualHardware,
    declaration: &PeripheralDeclaration,
    unix_seconds: u64,
) -> Result<Option<DeclaredDevice>, DeclarationError> {
    let Some(model) = Model::for_implementation(declaration.implementation) else {
        return Ok(None);
    };
    let bus = declaration
        .binding("i2c")
        .ok_or(DeclarationError::MissingBinding("i2c"))?;
    let declared = declaration
        .integer("address")?
        .ok_or(DeclarationError::MissingAddress)?;
    let address = u8::try_from(declared)
        .ok()
        .filter(|address| *address <= 0x7f)
        .ok_or(HardwareError::InvalidAddress(declared.unsigned_abs()))?;
    let device = model.device(declaration, unix_seconds)?;
    let model = device.model();
    hardware.attach_peripheral(bus, u64::from(address), device, declaration.name)?;
    Ok(Some(DeclaredDevice {
        bus,
        address,
        model,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::i2c::I2c as _;

    use super::*;
    use crate::Clock;

    const RTC: PeripheralDeclaration = PeripheralDeclaration {
        name: "real-time-clock",
        implementation: "rx8130ce-rtc",
        bindings: &[("i2c", "I2C2")],
        parameters: &[("address", "50")],
    };

    const MONITOR: PeripheralDeclaration = PeripheralDeclaration {
        name: "power-monitor",
        implementation: "ina226-power-monitor",
        bindings: &[("i2c", "I2C2")],
        parameters: &[("address", "64"), ("shunt-micro-ohms", "5000")],
    };

    fn hardware() -> VirtualHardware {
        VirtualHardware::new(&[], &["I2C0", "I2C2"], Clock::manual())
    }

    #[test]
    fn declared_implementations_get_their_chip_models() {
        let hardware = hardware();
        // 2026-10-08T12:34:56Z.
        let rtc = attach(&hardware, &RTC, 1_791_462_896).expect("RTC model");
        assert_eq!(
            rtc,
            Some(DeclaredDevice {
                bus: "I2C2",
                address: 0x32,
                model: "rx8130ce",
            })
        );
        let monitor = attach(&hardware, &MONITOR, 0).expect("monitor model");
        assert_eq!(monitor.map(|device| device.model), Some("ina226"));
        let devices = &hardware.buses()[1].devices;
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].peripheral.as_deref(), Some("real-time-clock"));
        assert_eq!(
            hardware
                .read_registers("I2C2", 0x32, 0x10, 7)
                .expect("clock registers"),
            [0x56, 0x34, 0x12, 1 << 4, 0x08, 0x10, 0x26]
        );
    }

    #[test]
    fn the_monitor_measures_the_modelled_supply() {
        let hardware = hardware();
        attach(&hardware, &MONITOR, 0).expect("monitor model");
        hardware
            .clock()
            .advance(std::time::Duration::from_millis(5));
        let mut bus = hardware.i2c_bus("I2C2").expect("bus");
        let mut word = [0; 2];
        bus.write_read(0x40, &[0x02], &mut word)
            .expect("bus voltage");
        assert_eq!(u16::from_be_bytes(word), 4_000, "5 V");
        bus.write_read(0x40, &[0x01], &mut word)
            .expect("shunt voltage");
        // 200 mA through 5 mΩ is 1 mV: 400 LSBs of 2.5 µV.
        assert_eq!(i16::from_be_bytes(word), 400);
        assert!(hardware.violations().is_empty());
    }

    #[test]
    fn unknown_implementations_and_bad_declarations_are_reported() {
        let hardware = hardware();
        let led = PeripheralDeclaration {
            name: "status-led",
            implementation: "indicator-led",
            bindings: &[("pin", "GPIO3")],
            parameters: &[("active-level", "low")],
        };
        assert!(attach(&hardware, &led, 0).expect("no model").is_none());
        let no_bus = PeripheralDeclaration {
            bindings: &[],
            ..RTC
        };
        assert!(matches!(
            attach(&hardware, &no_bus, 0),
            Err(DeclarationError::MissingBinding("i2c"))
        ));
        let bad_address = PeripheralDeclaration {
            parameters: &[("address", "0x32")],
            ..RTC
        };
        assert!(matches!(
            attach(&hardware, &bad_address, 0),
            Err(DeclarationError::InvalidParameter { .. })
        ));
        attach(&hardware, &RTC, 0).expect("first");
        assert!(matches!(
            attach(&hardware, &RTC, 0),
            Err(DeclarationError::Hardware(
                HardwareError::AddressInUse { .. }
            ))
        ));
    }
}

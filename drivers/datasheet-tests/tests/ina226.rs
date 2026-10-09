//! The driver against the INA226 datasheet model on a virtual I2C bus.

#![allow(clippy::expect_used)]

use std::time::Duration;

use barracuda_driver_ina226::Ina226;
use barracuda_platform_virtual_io::{Clock, VirtualHardware, models::Ina226Model};

const ADDRESS: u8 = 0x40;

fn hardware(model: Ina226Model) -> VirtualHardware {
    let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
    hardware
        .attach("I2C0", u64::from(ADDRESS), Box::new(model))
        .expect("attach INA226 model");
    hardware
}

#[test]
fn identity_matches_the_datasheet_register_values() {
    let hardware = hardware(Ina226Model::new());
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    let identity = Ina226::new(ADDRESS)
        .expect("address")
        .identity(&mut bus)
        .expect("identity");
    assert!(identity.is_ina226(), "{identity:?}");
    assert!(
        hardware.violations().is_empty(),
        "{:?}",
        hardware.violations()
    );
}

#[test]
fn raw_measurements_read_the_converted_registers() {
    // SBOS547C 6.5.1 example: 20 mV shunt drop (8000 × 2.5 µV) at 11.98 V
    // (9584 × 1.25 mV).
    let hardware = hardware(Ina226Model::new().with_inputs(-8_000, 9_584));
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    let chip = Ina226::new(ADDRESS).expect("address");

    // Before the first default conversion completes the registers hold
    // their power-on value; the driver reads them as they are.
    assert_eq!(chip.bus_voltage_raw(&mut bus).expect("bus voltage"), 0);

    hardware.clock().advance(Duration::from_millis(3));
    assert_eq!(chip.bus_voltage_raw(&mut bus).expect("bus voltage"), 9_584);
    assert_eq!(
        chip.shunt_voltage_raw(&mut bus).expect("shunt voltage"),
        -8_000
    );
    assert!(
        hardware.violations().is_empty(),
        "{:?}",
        hardware.violations()
    );
}

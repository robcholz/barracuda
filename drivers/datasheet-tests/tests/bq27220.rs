//! The driver against the BQ27220 datasheet model on a virtual I2C bus.

#![allow(clippy::expect_used)]

use std::time::Duration;

use barracuda_driver_bq27220::{Bq27220, DEFAULT_ADDRESS};
use barracuda_platform_virtual_io::{
    Clock, VirtualHardware,
    models::{Bq27220Model, bq27220::rules},
};

fn hardware() -> VirtualHardware {
    let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
    let model = Bq27220Model::new()
        .with_word(0x06, 2_981)
        .with_word(0x08, 3_925)
        .with_word(0x0c, (-125_i16) as u16)
        .with_word(0x10, 350)
        .with_word(0x12, 700)
        .with_word(0x2c, 50);
    hardware
        .attach("I2C0", u64::from(DEFAULT_ADDRESS), Box::new(model))
        .expect("attach BQ27220 model");
    hardware
}

#[test]
fn a_spaced_voltage_probe_reads_the_standard_command() {
    let hardware = hardware();
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    // SLUSCB7A 6.8: communicate after tPUCD (250 ms).
    hardware.clock().advance(Duration::from_millis(250));
    let chip = Bq27220::new(DEFAULT_ADDRESS).expect("address");
    assert_eq!(chip.voltage_millivolts(&mut bus).expect("voltage"), 3_925);
    assert!(
        hardware.violations().is_empty(),
        "{:?}",
        hardware.violations()
    );
}

#[test]
fn measure_reads_every_value_but_packs_packets_closer_than_t_buf() {
    let hardware = hardware();
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    hardware.clock().advance(Duration::from_millis(250));
    let measurement = Bq27220::new(DEFAULT_ADDRESS)
        .expect("address")
        .measure(&mut bus)
        .expect("measure");
    assert_eq!(measurement.voltage_millivolts, 3_925);
    assert_eq!(measurement.current_milliamps, -125);
    assert_eq!(measurement.temperature_decikelvin, 2_981);
    assert_eq!(measurement.remaining_capacity_milliamp_hours, 350);
    assert_eq!(measurement.full_charge_capacity_milliamp_hours, 700);
    assert_eq!(measurement.state_of_charge_percent, 50);

    // Open finding: measure() sends its six packets back to back. SLUSCB7A
    // 7.3.1.3 requires t(BUF) >= 66 us between all packets to the gauge, and
    // the driver has no delay to insert it. On the virtual bus, whose
    // transactions take no time, each of the five later packets breaks it.
    let rules: Vec<_> = hardware
        .violations()
        .into_iter()
        .map(|violation| violation.violation.rule)
        .collect();
    assert_eq!(rules, [rules::BUS_FREE; 5]);
}

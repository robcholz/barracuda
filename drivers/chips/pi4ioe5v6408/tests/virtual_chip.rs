//! The driver against the PI4IOE5V6408 datasheet model on a virtual I2C bus.

#![allow(clippy::expect_used)]

use std::time::Duration;

use barracuda_driver_pi4ioe5v6408::{Pi4ioe5v6408, Pull};
use barracuda_platform_virtual_io::{
    Clock, EventDetail, VirtualDelay, VirtualHardware, models::Pi4ioe5v6408Model,
};

const ADDRESS: u8 = 0x43;

fn hardware() -> VirtualHardware {
    let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
    hardware
        .attach(
            "I2C0",
            u64::from(ADDRESS),
            Box::new(Pi4ioe5v6408Model::new()),
        )
        .expect("attach PI4IOE5V6408 model");
    hardware
}

/// Pin changes noted by the model, with their time in microseconds.
fn pin_changes(hardware: &VirtualHardware) -> Vec<(u64, String)> {
    hardware
        .events(0)
        .into_iter()
        .filter_map(|event| match event.detail {
            EventDetail::Device { note, .. } => Some((event.at_us, note)),
            _ => None,
        })
        .collect()
}

#[test]
fn an_output_starts_at_its_initial_level_without_a_glitch() {
    // DS40583 c.iv: a pin whose high-impedance bit is 0 drives the Output
    // State register, whose power-up value is 0 (Table 2). Releasing the
    // high-impedance bit before writing the initial level would drive the
    // pin low first.
    let hardware = hardware();
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    let mut expander = Pi4ioe5v6408::new(&mut bus, ADDRESS).expect("expander");
    expander
        .configure_output(3, true)
        .expect("configure output");
    let changes: Vec<_> = pin_changes(&hardware)
        .into_iter()
        .map(|(_, note)| note)
        .collect();
    assert_eq!(changes, ["P3 high"]);
    assert!(
        hardware.violations().is_empty(),
        "{:?}",
        hardware.violations()
    );
}

#[test]
fn a_reset_pulse_holds_the_pin_low_for_the_requested_time() {
    let hardware = hardware();
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    let mut delay = VirtualDelay::new(hardware.clock().clone());
    let mut expander = Pi4ioe5v6408::new(&mut bus, ADDRESS).expect("expander");
    expander
        .pulse_reset_low(5, 10, 20, &mut delay)
        .expect("reset pulse");
    let changes = pin_changes(&hardware);
    assert_eq!(
        changes,
        [
            (0, String::from("P5 low")),
            (10_000, String::from("P5 high"))
        ]
    );
    assert_eq!(hardware.clock().now(), Duration::from_millis(30));
    assert!(
        hardware.violations().is_empty(),
        "{:?}",
        hardware.violations()
    );
}

#[test]
fn inputs_read_their_pulls_and_external_levels() {
    let hardware = hardware();
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    {
        let mut expander = Pi4ioe5v6408::new(&mut bus, ADDRESS).expect("expander");
        expander.configure_input(2, Pull::Up).expect("input");
        assert!(expander.input_is_high(2).expect("read"));
        expander.configure_input(4, Pull::None).expect("input");
    }
    // Drive P4 high from outside (backdoor offsets 20h level, 21h mask).
    hardware
        .write_registers("I2C0", u64::from(ADDRESS), 0x20, &[0x10, 0x10])
        .expect("drive P4");
    let mut expander = Pi4ioe5v6408::new(&mut bus, ADDRESS).expect("expander");
    assert!(expander.input_is_high(4).expect("read"));
    assert!(pin_changes(&hardware).is_empty(), "inputs never drive");
    assert!(
        hardware.violations().is_empty(),
        "{:?}",
        hardware.violations()
    );
}

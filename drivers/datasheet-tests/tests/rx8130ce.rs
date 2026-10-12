//! The driver against the RX8130CE datasheet model on a virtual I2C bus.

#![allow(clippy::expect_used)]

use std::time::Duration;

use barracuda_driver_rx8130ce::{ClockData, Rx8130ce};
use barracuda_platform_virtual_io::{
    Clock, VirtualHardware,
    models::{Rx8130ceModel, rx8130ce::rules},
};

const ADDRESS: u8 = 0x32;

fn hardware(model: Rx8130ceModel) -> VirtualHardware {
    let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
    hardware
        .attach("I2C0", u64::from(ADDRESS), Box::new(model))
        .expect("attach RX8130CE model");
    hardware
}

fn rule_ids(hardware: &VirtualHardware) -> Vec<String> {
    hardware
        .violations()
        .into_iter()
        .map(|violation| violation.violation.rule)
        .collect()
}

const CLOCK: ClockData = ClockData {
    year: 2026,
    month: 10,
    day: 7,
    weekday: 3,
    hour: 21,
    minute: 59,
    second: 58,
};

#[test]
fn a_kept_clock_reads_back_and_keeps_counting() {
    // 2026-10-07 (Wednesday, weekday 3) 21:59:58, VLF = 0.
    let hardware = hardware(Rx8130ceModel::running([
        0x58, 0x59, 0x21, 0x08, 0x07, 0x10, 0x26,
    ]));
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    let chip = Rx8130ce::new(ADDRESS).expect("address");
    chip.probe(&mut bus).expect("probe");
    assert!(!chip.lost_power(&mut bus).expect("flag"));
    assert_eq!(chip.read_clock(&mut bus).expect("clock"), CLOCK);
    hardware.clock().advance(Duration::from_secs(2 * 3600 + 2));
    let later = chip.read_clock(&mut bus).expect("clock");
    assert_eq!(
        (
            later.day,
            later.weekday,
            later.hour,
            later.minute,
            later.second
        ),
        (8, 4, 0, 0, 0)
    );
    assert!(
        rule_ids(&hardware).is_empty(),
        "{:?}",
        hardware.violations()
    );
}

#[test]
fn setting_the_clock_after_power_loss_follows_the_stop_procedure() {
    let hardware = hardware(Rx8130ceModel::new());
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    let chip = Rx8130ce::new(ADDRESS).expect("address");
    // ETM50E-10 10.1 Figure 14: VLF is readable 30 ms after power-on, and
    // the initial setting waits for t_str (max 1.0 s, Table 4).
    hardware.clock().advance(Duration::from_millis(30));
    assert!(chip.lost_power(&mut bus).expect("flag"));
    hardware.clock().advance(Duration::from_millis(970));
    chip.write_clock(&mut bus, CLOCK).expect("set clock");
    assert!(!chip.lost_power(&mut bus).expect("flag"));
    assert_eq!(chip.read_clock(&mut bus).expect("clock"), CLOCK);
    hardware.clock().advance(Duration::from_secs(1));
    assert_eq!(chip.read_clock(&mut bus).expect("clock").second, 59);

    // Open finding: write_clock clears VLF after writing only the clock and
    // Control Register0. ETM50E-10 13.2.1 and 14.5 ask that all registers be
    // initialized after VLF = 1; the alarm, timer, extension, Control
    // Register1, and digital-offset registers keep undefined values.
    assert_eq!(rule_ids(&hardware), [rules::INITIALIZE]);
}

#[test]
fn an_access_right_after_power_on_is_reported() {
    let hardware = hardware(Rx8130ceModel::new());
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    Rx8130ce::new(ADDRESS)
        .expect("address")
        .probe(&mut bus)
        .expect("probe");
    assert_eq!(rule_ids(&hardware), [rules::POWER_ON_ACCESS]);
}

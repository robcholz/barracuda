//! The driver against the ES8389 datasheet model on a virtual I2C bus.

#![allow(clippy::expect_used)]

use std::time::Duration;

use barracuda_driver_es8389::Es8389;
use barracuda_platform_virtual_io::{Clock, VirtualDelay, VirtualHardware, models::everest};

const ADDRESS: u8 = 0x10;

fn initialize(derive_clock_from_bclk: bool) -> VirtualHardware {
    let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
    hardware
        .attach("I2C0", u64::from(ADDRESS), Box::new(everest::es8389()))
        .expect("attach ES8389 model");
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    let mut delay = VirtualDelay::new(hardware.clock().clone());
    Es8389::new(ADDRESS)
        .expect("address")
        .initialize(&mut bus, derive_clock_from_bclk, &mut delay)
        .expect("initialize");
    hardware
}

fn register(hardware: &VirtualHardware, address: usize) -> u8 {
    hardware
        .read_registers("I2C0", u64::from(ADDRESS), address, 1)
        .expect("peek")[0]
}

#[test]
fn both_clock_paths_use_documented_registers_and_the_write_format() {
    for derive in [false, true] {
        let hardware = initialize(derive);
        assert!(
            hardware.violations().is_empty(),
            "{:?}",
            hardware.violations()
        );
        assert_eq!(hardware.clock().now(), Duration::from_millis(10));
        assert_eq!(register(&hardware, 0x02), if derive { 0x40 } else { 0x00 });
        assert_eq!(register(&hardware, 0x64), 0x8f);
    }
}

#[test]
fn initialization_writes_registers_the_datasheet_lists_as_reserved() {
    // Observation, not a rule: rev 5.0 section 8 lists 0x12-0x1C, 0x30, and
    // 0x4C as reserved, and the initialization tables write them. The
    // datasheet does not forbid it, so the model stores them.
    let hardware = initialize(false);
    assert_eq!(register(&hardware, 0x16), 0x35);
    assert_eq!(register(&hardware, 0x30), 0xf4);
    assert_eq!(register(&hardware, 0x4c), 0xc0);
}

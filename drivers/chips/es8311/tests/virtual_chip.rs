//! The driver against the ES8311 datasheet model on a virtual I2C bus.

#![allow(clippy::expect_used)]

use std::time::Duration;

use barracuda_driver_es8311::Es8311;
use barracuda_platform_virtual_io::{Clock, VirtualDelay, VirtualHardware, models::everest};

const ADDRESS: u8 = 0x18;

#[test]
fn initialization_programs_documented_registers_in_the_write_format() {
    let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
    hardware
        .attach("I2C0", u64::from(ADDRESS), Box::new(everest::es8311()))
        .expect("attach ES8311 model");
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    let mut delay = VirtualDelay::new(hardware.clock().clone());
    let codec = Es8311::new(ADDRESS).expect("address");
    codec.initialize(&mut bus, &mut delay).expect("initialize");
    codec.set_dac_volume(&mut bus, 0xa0).expect("volume");

    assert!(
        hardware.violations().is_empty(),
        "{:?}",
        hardware.violations()
    );
    assert_eq!(hardware.clock().now(), Duration::from_millis(20));
    let register = |address: usize| {
        hardware
            .read_registers("I2C0", u64::from(ADDRESS), address, 1)
            .expect("peek")[0]
    };
    // Register 0x00: CSM_ON set, every block out of reset (section 8).
    assert_eq!(register(0x00), 0x80);
    assert_eq!(register(0x01), 0x3f);
    assert_eq!(register(0x32), 0xa0);
    assert_eq!(register(0x37), 0x08);
}

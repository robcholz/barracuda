//! The driver against the ES7210 datasheet model on a virtual I2C bus.

#![allow(clippy::expect_used)]

use barracuda_driver_es7210::Es7210;
use barracuda_platform_virtual_io::{Clock, VirtualHardware, models::everest};

const ADDRESS: u8 = 0x40;

#[test]
fn initialization_programs_documented_registers_in_the_write_format() {
    let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
    hardware
        .attach("I2C0", u64::from(ADDRESS), Box::new(everest::es7210()))
        .expect("attach ES7210 model");
    let mut bus = hardware.i2c_bus("I2C0").expect("bus");
    Es7210::new(ADDRESS)
        .expect("address")
        .initialize(&mut bus)
        .expect("initialize");

    assert!(
        hardware.violations().is_empty(),
        "{:?}",
        hardware.violations()
    );
    let register = |address: usize| {
        hardware
            .read_registers("I2C0", u64::from(ADDRESS), address, 1)
            .expect("peek")[0]
    };
    assert_eq!(register(0x00), 0x41);
    assert_eq!(register(0x4b), 0x0f);
    assert_eq!(register(0x11), 0x60);
}

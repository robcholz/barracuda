//! The encoder against the WS2812B datasheet model on a virtual SPI bus.

#![allow(clippy::expect_used)]

use barracuda_driver_ws2812_spi::{ColorOrder, Ws2812Spi};
use barracuda_platform_virtual_io::{
    Clock, VirtualHardware,
    models::{Ws2812bModel, ws2812b::rules},
};

fn hardware() -> VirtualHardware {
    let hardware = VirtualHardware::new(&[], &[], Clock::manual()).with_spi(&["SPI2"]);
    hardware
        .attach_spi("SPI2", Box::new(Ws2812bModel::new(3)))
        .expect("attach WS2812B model");
    hardware
}

#[test]
fn colors_reach_the_chain_in_grb_order_with_valid_bit_timing() {
    let hardware = hardware();
    // The encoder is documented for a 2.4 MHz bus: three SPI bits per
    // WS2812B bit (0 = 100, 1 = 110), 416.7 ns each.
    let bus = hardware.spi_bus("SPI2", 2_400_000).expect("bus");
    let mut chain = Ws2812Spi::new(bus, 3, ColorOrder::Grb)
        .map_err(|_bus| "empty chain")
        .expect("chain");
    chain
        .write_rgb([(0xff, 0x00, 0x00), (0x01, 0x80, 0x7f)].into_iter())
        .expect("write");
    assert!(
        hardware.violations().is_empty(),
        "{:?}",
        hardware.violations()
    );
    assert_eq!(
        hardware.read_spi_registers("SPI2", 0, 9).expect("peek"),
        [0x00, 0xff, 0x00, 0x80, 0x01, 0x7f, 0, 0, 0],
        "first LED red, second (r=1, g=128, b=127), third blanked"
    );
}

#[test]
fn a_bus_clock_other_than_2_4_mhz_breaks_the_code_windows() {
    let hardware = hardware();
    let bus = hardware.spi_bus("SPI2", 4_000_000).expect("bus");
    let mut chain = Ws2812Spi::new(bus, 3, ColorOrder::Grb)
        .map_err(|_bus| "empty chain")
        .expect("chain");
    chain.write_rgb([(1, 2, 3)].into_iter()).expect("write");
    assert!(
        hardware
            .violations()
            .iter()
            .all(|violation| violation.violation.rule == rules::BIT_TIMING)
    );
    assert!(!hardware.violations().is_empty());
}

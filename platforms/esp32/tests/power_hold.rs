//! Projection of a Board's battery power latch into the entry macro.

#![allow(clippy::expect_used, missing_docs)]

#[path = "../build_power_hold.rs"]
mod power_hold;

#[test]
fn a_latch_pin_is_driven_by_the_entry() {
    let rust = power_hold::power_hold_macro(Some("GPIO4")).expect("macro");
    assert!(rust.contains("macro_rules! __barracuda_power_hold"));
    assert!(rust.contains("$crate::application::hold_power($peripherals.GPIO4)"));
}

#[test]
fn a_board_without_a_latch_gets_a_macro_that_does_nothing() {
    let rust = power_hold::power_hold_macro(None).expect("macro");
    assert!(rust.contains("($peripherals:ident) => {  };"));
    assert!(!rust.contains("hold_power"));
}

#[test]
fn only_a_chip_pin_name_reaches_the_generated_source() {
    assert!(power_hold::power_hold_macro(Some("GPIO4); loop {}; (")).is_err());
    assert!(power_hold::power_hold_macro(Some("")).is_err());
}

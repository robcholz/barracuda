//! STM32 linker-region boundary tests.

#![allow(clippy::expect_used)]

use barracuda_platform_stm32::{LinkerRegion, LinkerRegionError};

#[test]
fn linker_bounds_become_flash_relative_regions() {
    let region = LinkerRegion::try_from_addresses(0x0800_0000, 0x081a_0000, 0x0820_0000)
        .expect("valid linker region");

    assert_eq!(region.offset(), 0x001a_0000);
    assert_eq!(region.size(), 0x0006_0000);
}

#[test]
fn linker_bounds_reject_reversed_or_pre_flash_symbols() {
    assert_eq!(
        LinkerRegion::try_from_addresses(0x0800_0000, 0x07ff_0000, 0x0820_0000),
        Err(LinkerRegionError::BeforeFlash)
    );
    assert_eq!(
        LinkerRegion::try_from_addresses(0x0800_0000, 0x0820_0000, 0x081f_0000),
        Err(LinkerRegionError::Reversed)
    );
}

//! ESP-IDF partition-table projection tests.

#![allow(clippy::expect_used)]

use barracuda_platform_esp32::{Esp32RegionAccess, BOARD_ESP32_LAYOUT};

#[test]
fn native_csv_supplies_ota_and_bound_storage_regions() {
    assert_eq!(BOARD_ESP32_LAYOUT.chip(), "esp32c6");
    assert_eq!(BOARD_ESP32_LAYOUT.ota_slot_count(), 2);
    assert_eq!(BOARD_ESP32_LAYOUT.database().name(), "database");
    assert_eq!(BOARD_ESP32_LAYOUT.database().offset(), 0x42_0000);
    assert_eq!(BOARD_ESP32_LAYOUT.filesystem().name(), "filesystem");

    let assets = BOARD_ESP32_LAYOUT.web_assets().expect("Web asset region");
    assert_eq!(assets.name(), "web-assets");
    assert_eq!(assets.access(), Esp32RegionAccess::ReadOnly);
}

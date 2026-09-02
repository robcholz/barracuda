//! ESP-IDF partition-table projection tests.

#![allow(clippy::expect_used)]

use barracuda_platform_esp32c6::{
    Esp32c6PartitionTable, Esp32c6Region, Esp32c6RegionAccess, BOARD_ESP32C6_PARTITION_TABLE,
};

#[test]
fn public_partition_table_preserves_native_metadata_and_lookup() {
    static REGIONS: [Esp32c6Region; 2] = [
        Esp32c6Region::new("config", 0x1000, 0x2000, Esp32c6RegionAccess::ReadWrite),
        Esp32c6Region::new("assets", 0x3000, 0x4000, Esp32c6RegionAccess::ReadOnly),
    ];
    let table = Esp32c6PartitionTable::new("fixture", 1, &REGIONS);
    assert_eq!(table.chip(), "fixture");
    assert_eq!(table.ota_slot_count(), 1);
    assert_eq!(table.regions(), &REGIONS);
    assert_eq!(table.get("config"), Some(&REGIONS[0]));
    assert_eq!(table.get("missing"), None);
    assert_eq!(REGIONS[0].name(), "config");
    assert_eq!(REGIONS[0].offset(), 0x1000);
    assert_eq!(REGIONS[0].size(), 0x2000);
    assert_eq!(REGIONS[0].access(), Esp32c6RegionAccess::ReadWrite);
}

#[test]
fn native_csv_supplies_the_complete_partition_table() {
    assert_eq!(BOARD_ESP32C6_PARTITION_TABLE.chip(), "esp32c6");
    if BOARD_ESP32C6_PARTITION_TABLE.regions().is_empty() {
        assert_eq!(BOARD_ESP32C6_PARTITION_TABLE.ota_slot_count(), 0);
        return;
    }
    assert_eq!(BOARD_ESP32C6_PARTITION_TABLE.ota_slot_count(), 2);
    assert_eq!(BOARD_ESP32C6_PARTITION_TABLE.regions().len(), 8);
    assert!(BOARD_ESP32C6_PARTITION_TABLE.get("nvs").is_some());
    assert!(BOARD_ESP32C6_PARTITION_TABLE.get("ota_0").is_some());
    assert_eq!(
        BOARD_ESP32C6_PARTITION_TABLE
            .get("kv_database")
            .expect("KV database region")
            .offset(),
        0x42_0000
    );
    assert_eq!(
        BOARD_ESP32C6_PARTITION_TABLE
            .get("system")
            .expect("System region")
            .offset(),
        0x52_0000
    );

    let assets = BOARD_ESP32C6_PARTITION_TABLE
        .get("web_assets")
        .expect("Web asset region");
    assert_eq!(assets.name(), "web_assets");
    assert_eq!(assets.access(), Esp32c6RegionAccess::ReadOnly);
}

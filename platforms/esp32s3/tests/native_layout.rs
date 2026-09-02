//! ESP-IDF partition-table projection tests.

#![allow(clippy::expect_used)]

use barracuda_platform_esp32s3::{
    Esp32S3PartitionTable, Esp32S3Region, Esp32S3RegionAccess, BOARD_ESP32S3_PARTITION_TABLE,
};

#[test]
fn public_partition_table_preserves_native_metadata_and_lookup() {
    static REGIONS: [Esp32S3Region; 2] = [
        Esp32S3Region::new("config", 0x1000, 0x2000, Esp32S3RegionAccess::ReadWrite),
        Esp32S3Region::new("assets", 0x3000, 0x4000, Esp32S3RegionAccess::ReadOnly),
    ];
    let table = Esp32S3PartitionTable::new("fixture", 1, &REGIONS);
    assert_eq!(table.chip(), "fixture");
    assert_eq!(table.ota_slot_count(), 1);
    assert_eq!(table.regions(), &REGIONS);
    assert_eq!(table.get("config"), Some(&REGIONS[0]));
    assert_eq!(table.get("missing"), None);
    assert_eq!(REGIONS[0].name(), "config");
    assert_eq!(REGIONS[0].offset(), 0x1000);
    assert_eq!(REGIONS[0].size(), 0x2000);
    assert_eq!(REGIONS[0].access(), Esp32S3RegionAccess::ReadWrite);
}

#[test]
fn native_csv_supplies_the_complete_partition_table() {
    assert_eq!(BOARD_ESP32S3_PARTITION_TABLE.chip(), "esp32s3");
    if BOARD_ESP32S3_PARTITION_TABLE.regions().is_empty() {
        assert_eq!(BOARD_ESP32S3_PARTITION_TABLE.ota_slot_count(), 0);
        return;
    }
    assert_eq!(BOARD_ESP32S3_PARTITION_TABLE.ota_slot_count(), 2);
    assert_eq!(BOARD_ESP32S3_PARTITION_TABLE.regions().len(), 8);
    assert!(BOARD_ESP32S3_PARTITION_TABLE.get("nvs").is_some());
    assert!(BOARD_ESP32S3_PARTITION_TABLE.get("ota_0").is_some());
    assert_eq!(
        BOARD_ESP32S3_PARTITION_TABLE
            .get("kv_database")
            .expect("KV database region")
            .offset(),
        0x2e_0000
    );
    assert_eq!(
        BOARD_ESP32S3_PARTITION_TABLE
            .get("system")
            .expect("System region")
            .offset(),
        0x32_0000
    );

    let assets = BOARD_ESP32S3_PARTITION_TABLE
        .get("web_assets")
        .expect("Web asset region");
    assert_eq!(assets.name(), "web_assets");
    assert_eq!(assets.access(), Esp32S3RegionAccess::ReadOnly);
}

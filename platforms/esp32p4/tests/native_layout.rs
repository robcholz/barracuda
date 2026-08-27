//! ESP-IDF partition-table projection tests.

#![allow(clippy::expect_used)]

use barracuda_platform_esp32p4::{Esp32P4RegionAccess, BOARD_ESP32P4_PARTITION_TABLE};

#[test]
fn native_csv_supplies_the_complete_partition_table() {
    assert_eq!(BOARD_ESP32P4_PARTITION_TABLE.chip(), "esp32p4");
    assert_eq!(BOARD_ESP32P4_PARTITION_TABLE.ota_slot_count(), 2);
    assert_eq!(BOARD_ESP32P4_PARTITION_TABLE.regions().len(), 8);
    assert!(BOARD_ESP32P4_PARTITION_TABLE.get("nvs").is_some());
    assert!(BOARD_ESP32P4_PARTITION_TABLE.get("ota_0").is_some());
    assert_eq!(
        BOARD_ESP32P4_PARTITION_TABLE
            .get("kv_database")
            .expect("KV database region")
            .offset(),
        0x42_0000
    );
    assert_eq!(
        BOARD_ESP32P4_PARTITION_TABLE
            .get("system")
            .expect("System region")
            .offset(),
        0x52_0000
    );

    let assets = BOARD_ESP32P4_PARTITION_TABLE
        .get("web_assets")
        .expect("Web asset region");
    assert_eq!(assets.name(), "web_assets");
    assert_eq!(assets.access(), Esp32P4RegionAccess::ReadOnly);
}

#[test]
fn build_script_does_not_select_system_storage_roles() -> Result<(), std::io::Error> {
    let source =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs"))?;
    for forbidden in [
        "board.storage()",
        "filesystem()",
        "database()",
        "web_assets()",
    ] {
        assert!(
            !source.contains(forbidden),
            "found business selector `{forbidden}`"
        );
    }
    Ok(())
}

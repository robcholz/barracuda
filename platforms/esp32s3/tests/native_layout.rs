//! ESP-IDF partition-table projection tests.

#![allow(clippy::expect_used)]

use barracuda_platform::PartitionFilesystem;
use barracuda_platform_esp32s3::{Esp32S3RegionAccess, BOARD_ESP32S3_PARTITION_TABLE};

#[test]
fn native_csv_supplies_the_complete_partition_table() {
    assert_eq!(BOARD_ESP32S3_PARTITION_TABLE.chip(), "esp32s3");
    if BOARD_ESP32S3_PARTITION_TABLE.regions().is_empty() {
        assert_eq!(BOARD_ESP32S3_PARTITION_TABLE.ota_slot_count(), 0);
        return;
    }
    // One app slot on Boards with less than 16 MiB of flash, two on the
    // others; the release image and `cargo run` write `ota_0`.
    let slots = BOARD_ESP32S3_PARTITION_TABLE.ota_slot_count();
    assert!(matches!(slots, 1 | 2), "{slots} app slots");
    assert_eq!(BOARD_ESP32S3_PARTITION_TABLE.regions().len(), 6 + slots);
    for name in ["nvs", "otadata", "phy_init", "ota_0"] {
        assert!(
            BOARD_ESP32S3_PARTITION_TABLE.get(name).is_some(),
            "no `{name}`"
        );
    }
    assert_eq!(
        BOARD_ESP32S3_PARTITION_TABLE.get("ota_1").is_some(),
        slots == 2
    );

    // The data regions follow the last app slot, so the release image, which
    // ends in `ota_0`, leaves them alone.
    let last_slot = BOARD_ESP32S3_PARTITION_TABLE
        .get(if slots == 2 { "ota_1" } else { "ota_0" })
        .expect("last app slot");
    let database = BOARD_ESP32S3_PARTITION_TABLE
        .get("kv_database")
        .expect("KV database region");
    assert_eq!(database.offset(), last_slot.offset() + last_slot.size());
    let system = BOARD_ESP32S3_PARTITION_TABLE
        .get("system")
        .expect("System region");
    assert_eq!(system.offset(), database.offset() + database.size());

    let resources = BOARD_ESP32S3_PARTITION_TABLE
        .get("resources")
        .expect("Plugin resources region");
    assert_eq!(resources.name(), "resources");
    assert_eq!(resources.access(), Esp32S3RegionAccess::ReadOnly);
    assert_eq!(resources.filesystem(), PartitionFilesystem::FatFs);
}

#[test]
fn build_script_does_not_select_system_storage_roles() -> Result<(), std::io::Error> {
    let source =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs"))?;
    for forbidden in [
        "board.storage()",
        "filesystem()",
        "database()",
        "resources()",
    ] {
        assert!(
            !source.contains(forbidden),
            "found business selector `{forbidden}`"
        );
    }
    Ok(())
}

//! The app slots every ESP Platform's build script accepts in a native table.

#![allow(clippy::expect_used, missing_docs)]

#[path = "../build_app_slots.rs"]
mod app_slots;

use esp_idf_part::PartitionTable;

const HEAD: &str = "\
nvs,          data, nvs,     0x9000,   0x6000,
otadata,      data, ota,     0xf000,   0x2000,
phy_init,     data, phy,     0x11000,  0x1000,
";
const DATA: &str = "\
kv_database,  data, 0x80,    0x2e0000, 0x40000,
system,       data, littlefs,0x320000, 0x80000,
resources,    data, fat,     0x3a0000, 0x60000, readonly
";

fn slots(apps: &str) -> Result<usize, String> {
    slots_in(&format!("{HEAD}{apps}{DATA}"))
}

fn slots_in(csv: &str) -> Result<usize, String> {
    let table = PartitionTable::try_from_str(csv).expect("partition table");
    table.validate().expect("valid partition table");
    app_slots::app_slot_count(&table)
}

#[test]
fn a_four_megabyte_board_has_one_slot() {
    assert_eq!(slots("ota_0, app, ota_0, 0x20000, 0x2c0000,\n"), Ok(1));
}

#[test]
fn a_sixteen_megabyte_board_has_two_slots() {
    assert_eq!(
        slots("ota_0, app, ota_0, 0x20000, 0x160000,\nota_1, app, ota_1, 0x180000, 0x160000,\n"),
        Ok(2)
    );
}

#[test]
fn a_factory_app_would_boot_instead_of_ota_0() {
    let error = slots("factory, app, factory, 0x20000, 0x2c0000,\n").expect_err("factory");
    assert!(error.contains("`factory`"), "{error}");
    let error = slots(
        "factory, app, factory, 0x20000, 0x160000,\nota_0, app, ota_0, 0x180000, 0x160000,\n",
    )
    .expect_err("factory beside ota_0");
    assert!(error.contains("`factory`"), "{error}");
}

#[test]
fn slots_start_at_ota_0_without_a_gap() {
    let error = slots("ota_1, app, ota_1, 0x20000, 0x2c0000,\n").expect_err("no ota_0");
    assert!(error.contains("`ota_1`"), "{error}");
    let error =
        slots("ota_0, app, ota_0, 0x20000, 0x160000,\nota_2, app, ota_2, 0x180000, 0x160000,\n")
            .expect_err("gap");
    assert!(error.contains("`ota_0`, `ota_2`"), "{error}");
}

#[test]
fn the_boot_loader_needs_otadata() {
    let error = slots_in(&format!(
        "nvs, data, nvs, 0x9000, 0x6000,\nphy_init, data, phy, 0x11000, 0x1000,\n\
         ota_0, app, ota_0, 0x20000, 0x2c0000,\n{DATA}"
    ))
    .expect_err("no otadata");
    assert!(error.contains("`otadata`"), "{error}");
}

//! Selected-Board System image behavior.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use barracuda_system_image::{build_selected, selected_system_region};
use tempfile::tempdir;

fn select_board(root: &Path, name: &str, board: &str, artifact: &str, layout: &str) {
    let bundle = root.join("boards/configs").join(name);
    fs::create_dir_all(&bundle).expect("Board bundle");
    fs::create_dir_all(root.join(".barracuda")).expect("selection directory");
    fs::write(root.join(".barracuda/selected-board"), format!("{name}\n")).expect("selected Board");
    fs::write(bundle.join("board.yml"), board).expect("Board definition");
    fs::write(bundle.join(artifact), layout).expect("native layout");
}

#[test]
fn resolves_system_region_from_selected_file_layout_board() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "local-test",
        "name: local-test\nhardware:\n  chip: macos\nnative-layout:\n  artifact: file-layout.yml\n",
        "file-layout.yml",
        "capacity: 4194304\nregions:\n  - name: system\n    offset: 4096\n    size: 2097152\n    access: read-write\n",
    );

    let region = selected_system_region(root.path()).expect("selected system region");

    assert_eq!(region.board(), "local-test");
    assert_eq!(region.offset(), 4096);
    assert_eq!(region.size(), 2_097_152);
}

#[test]
fn resolves_system_region_from_selected_esp_partition_table() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "esp-test",
        "name: esp-test\nhardware:\n  chip: esp32c6\ntoolchain:\n  target: riscv32imac-unknown-none-elf\nnative-layout:\n  artifact: partitions.csv\n",
        "partitions.csv",
        "# Name, Type, SubType, Offset, Size, Flags\nfactory, app, factory, 0x10000, 0x100000,\nsystem, data, littlefs, 0x320000, 0x80000,\n",
    );

    let region = selected_system_region(root.path()).expect("selected system region");

    assert_eq!(region.offset(), 0x32_0000);
    assert_eq!(region.size(), 0x8_0000);
}

#[test]
fn resolves_system_region_from_selected_stm32_linker_layout() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "stm-test",
        "name: stm-test\nhardware:\n  chip: stm32f429zi\ntoolchain:\n  target: thumbv7em-none-eabihf\nnative-layout:\n  artifact: memory.x\n",
        "memory.x",
        "MEMORY\n{\n  SYSTEM (rw) : ORIGIN = 0x08120000, LENGTH = 256K\n}\n",
    );

    let region = selected_system_region(root.path()).expect("selected system region");

    assert_eq!(region.offset(), 0x0812_0000);
    assert_eq!(region.size(), 256 * 1024);
}

#[test]
fn requires_a_selected_board_and_a_system_region() {
    let root = tempdir().expect("temporary workspace");
    assert!(selected_system_region(root.path())
        .expect_err("selection is required")
        .contains("cargo board select"));

    select_board(
        root.path(),
        "local-test",
        "name: local-test\nhardware:\n  chip: linux\nnative-layout:\n  artifact: file-layout.yml\n",
        "file-layout.yml",
        "capacity: 4096\nregions: []\n",
    );
    assert!(selected_system_region(root.path())
        .expect_err("system region is required")
        .contains("system"));
}

#[test]
fn builds_only_from_the_top_level_image_directory() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "local-test",
        "name: local-test\nhardware:\n  chip: linux\nnative-layout:\n  artifact: file-layout.yml\n",
        "file-layout.yml",
        "capacity: 131072\nregions:\n  - name: system\n    offset: 0\n    size: 65536\n    access: read-write\n",
    );
    fs::create_dir(root.path().join("image")).expect("top-level image directory");

    let built = build_selected(root.path()).expect("build selected image");

    assert_eq!(built.source(), root.path().join("image"));
    assert_eq!(
        built.output(),
        root.path().join("target/barracuda-system.img")
    );
    assert_eq!(built.board(), "local-test");
    assert_eq!(built.offset(), 0);
    assert_eq!(built.size(), 65_536);
    assert_eq!(
        fs::metadata(built.output()).expect("output metadata").len(),
        65_536
    );
}

#[test]
fn rejects_mismatched_and_unsupported_board_bundles() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "directory-name",
        "name: declared-name\nhardware:\n  chip: linux\nnative-layout:\n  artifact: file-layout.yml\n",
        "file-layout.yml",
        "capacity: 65536\nregions: []\n",
    );
    assert!(selected_system_region(root.path())
        .expect_err("name mismatch")
        .contains("declares Board"));

    select_board(
        root.path(),
        "unknown-test",
        "name: unknown-test\nhardware:\n  chip: unknown\nnative-layout:\n  artifact: layout.bin\n",
        "layout.bin",
        "unused",
    );
    assert!(selected_system_region(root.path())
        .expect_err("unsupported layout")
        .contains("no Barracuda Platform supports Board chip"));
}

#[test]
fn rejects_a_board_whose_toolchain_selects_an_incompatible_platform() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "esp-test",
        "name: esp-test\nhardware:\n  chip: esp32c6\ntoolchain:\n  target: riscv32imafc-unknown-none-elf\nnative-layout:\n  artifact: partitions.csv\n",
        "partitions.csv",
        "# Name, Type, SubType, Offset, Size, Flags\nsystem, data, littlefs, 0x320000, 0x80000,\n",
    );

    let error = selected_system_region(root.path()).expect_err("Platform mismatch must fail");

    assert!(error.contains("esp32c6"));
    assert!(error.contains("esp32p4"));
}

#[test]
fn rejects_invalid_file_layout_system_regions() {
    let root = tempdir().expect("temporary workspace");
    let board =
        "name: local-test\nhardware:\n  chip: macos\nnative-layout:\n  artifact: file-layout.yml\n";
    select_board(
        root.path(),
        "local-test",
        board,
        "file-layout.yml",
        "capacity: 65536\nregions:\n  - name: system\n    offset: 0\n    size: 65536\n    access: read-only\n",
    );
    assert!(selected_system_region(root.path())
        .expect_err("read-only system region")
        .contains("read-only"));

    select_board(
        root.path(),
        "local-test",
        board,
        "file-layout.yml",
        "capacity: 65536\nregions:\n  - name: system\n    offset: 4096\n    size: 65536\n    access: read-write\n",
    );
    assert!(selected_system_region(root.path())
        .expect_err("out-of-range system region")
        .contains("exceeds"));
}

#[test]
fn rejects_invalid_stm32_system_values() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "stm-test",
        "name: stm-test\nhardware:\n  chip: stm32f429zi\ntoolchain:\n  target: thumbv7em-none-eabihf\nnative-layout:\n  artifact: memory.x\n",
        "memory.x",
        "MEMORY\n{\n  SYSTEM (rw) : ORIGIN = nope, LENGTH = 256K\n}\n",
    );

    assert!(selected_system_region(root.path())
        .expect_err("invalid STM32 origin")
        .contains("invalid STM32 layout value"));
}

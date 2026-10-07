//! Selected-Board System image behavior.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use barracuda_system_image::{
    build_selected, build_workspace, selected_resources_region, ResourcesFilesystem,
};
use tempfile::tempdir;

fn select_board(root: &Path, name: &str, board: &str, artifact: &str, layout: &str) {
    copy_platform_catalog(root);
    let bundle = root.join("boards/configs").join(name);
    fs::create_dir_all(&bundle).expect("Board bundle");
    fs::create_dir_all(root.join(".barracuda")).expect("selection directory");
    fs::write(root.join(".barracuda/selected-board"), format!("{name}\n")).expect("selected Board");
    fs::write(bundle.join("board.yml"), board).expect("Board definition");
    fs::write(bundle.join(artifact), layout).expect("native layout");
}

fn copy_platform_catalog(root: &Path) {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let catalog = fs::read_dir(workspace.join("platforms")).expect("Platform catalog");
    for entry in catalog {
        let entry = entry.expect("Platform entry");
        let source = entry.path().join("platform.yml");
        if !source.is_file() {
            continue;
        }
        let destination = root.join("platforms").join(entry.file_name());
        fs::create_dir_all(&destination).expect("Platform destination");
        fs::copy(source, destination.join("platform.yml")).expect("Platform manifest");
    }
}

#[test]
fn resolves_resources_region_from_selected_file_layout_board() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "local-test",
        "name: local-test\nhardware:\n  chip: macos\nnative-layout:\n  artifact: file-layout.yml\n",
        "file-layout.yml",
        "capacity: 4194304\nregions:\n  - name: system\n    offset: 4096\n    size: 2097152\n    access: read-write\n    filesystem: raw\n  - name: resources\n    offset: 2101248\n    size: 1048576\n    access: read-only\n    filesystem: fatfs\n",
    );

    let region = selected_resources_region(root.path()).expect("selected resources region");

    assert_eq!(region.board(), "local-test");
    assert_eq!(region.offset(), 2_101_248);
    assert_eq!(region.size(), 1_048_576);
    assert_eq!(region.filesystem(), ResourcesFilesystem::FatFs);
}

#[test]
fn resolves_resources_region_from_selected_esp_partition_table() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "esp-test",
        "name: esp-test\nhardware:\n  chip: esp32c6\ntoolchain:\n  target: riscv32imac-unknown-none-elf\nnative-layout:\n  artifact: partitions.csv\n",
        "partitions.csv",
        "# Name, Type, SubType, Offset, Size, Flags\nfactory, app, factory, 0x10000, 0x100000,\nsystem, data, littlefs, 0x320000, 0x80000,\nresources, data, fat, 0x3a0000, 0x60000, readonly\n",
    );

    let region = selected_resources_region(root.path()).expect("selected resources region");

    assert_eq!(region.offset(), 0x3a_0000);
    assert_eq!(region.size(), 0x6_0000);
    assert_eq!(region.filesystem(), ResourcesFilesystem::FatFs);
}

#[test]
fn resolves_resources_region_from_selected_stm32_linker_layout() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "stm-test",
        "name: stm-test\nhardware:\n  chip: stm32u5a5zj\ntoolchain:\n  target: thumbv8m.main-none-eabihf\nnative-layout:\n  artifact: memory.x\n",
        "memory.x",
        "MEMORY\n{\n  SYSTEM (rw) : ORIGIN = 0x08120000, LENGTH = 256K\n  RESOURCES (r) : ORIGIN = 0x08160000, LENGTH = 256K /* filesystem: fatfs */\n}\n",
    );

    let region = selected_resources_region(root.path()).expect("selected resources region");

    assert_eq!(region.offset(), 0x0816_0000);
    assert_eq!(region.size(), 256 * 1024);
    assert_eq!(region.filesystem(), ResourcesFilesystem::FatFs);
}

#[test]
fn requires_a_selected_board_and_a_resources_region() {
    let root = tempdir().expect("temporary workspace");
    assert!(selected_resources_region(root.path())
        .expect_err("selection is required")
        .contains("cargo board select"));

    select_board(
        root.path(),
        "local-test",
        "name: local-test\nhardware:\n  chip: linux\nnative-layout:\n  artifact: file-layout.yml\n",
        "file-layout.yml",
        "capacity: 4096\nregions: []\n",
    );
    assert!(selected_resources_region(root.path())
        .expect_err("resources region is required")
        .contains("resources"));
}

#[test]
fn builds_the_selected_workspace_image() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "local-test",
        "name: local-test\nhardware:\n  chip: linux\nnative-layout:\n  artifact: file-layout.yml\n",
        "file-layout.yml",
        "capacity: 1048576\nregions:\n  - name: resources\n    offset: 262144\n    size: 524288\n    access: read-only\n    filesystem: littlefs\n",
    );
    let plugin = root.path().join("plugins/demo");
    fs::create_dir_all(plugin.join("filesystem/workspace/resources/models"))
        .expect("shared Plugin resources");
    fs::write(
        plugin.join("plugin.toml"),
        "id = \"demo\"\ndepends-on = []\ndescription = \"Test Plugin.\"\n",
    )
    .expect("Plugin manifest");
    fs::write(
        plugin.join("filesystem/workspace/resources/models/common.txt"),
        b"shared",
    )
    .expect("shared Plugin resource");
    let expected = build_workspace(root.path(), 524_288, ResourcesFilesystem::LittleFs)
        .expect("expected selected image");

    let built = build_selected(root.path()).expect("build selected image");

    assert_eq!(
        built.output(),
        root.path().join("target/barracuda-system.img")
    );
    assert_eq!(built.board(), "local-test");
    assert_eq!(built.offset(), 262_144);
    assert_eq!(built.size(), 524_288);
    assert_eq!(built.filesystem(), ResourcesFilesystem::LittleFs);
    assert_eq!(
        fs::metadata(built.output()).expect("output metadata").len(),
        524_288
    );
    assert_eq!(fs::read(built.output()).expect("selected image"), expected);
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
    assert!(selected_resources_region(root.path())
        .expect_err("name mismatch")
        .contains("declares Board"));

    select_board(
        root.path(),
        "unknown-test",
        "name: unknown-test\nhardware:\n  chip: unknown\nnative-layout:\n  artifact: layout.bin\n",
        "layout.bin",
        "unused",
    );
    assert!(selected_resources_region(root.path())
        .expect_err("unsupported layout")
        .contains("no Platform matches Board chip `unknown`"));
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

    let error = selected_resources_region(root.path()).expect_err("Platform mismatch must fail");

    assert!(error.contains("esp32c6"));
    assert!(error.contains("riscv32imafc-unknown-none-elf"));
}

#[test]
fn rejects_invalid_file_layout_resources_regions() {
    let root = tempdir().expect("temporary workspace");
    let board =
        "name: local-test\nhardware:\n  chip: macos\nnative-layout:\n  artifact: file-layout.yml\n";
    select_board(
        root.path(),
        "local-test",
        board,
        "file-layout.yml",
        "capacity: 65536\nregions:\n  - name: resources\n    offset: 0\n    size: 65536\n    access: read-write\n    filesystem: raw\n",
    );
    assert!(selected_resources_region(root.path())
        .expect_err("writable resources region")
        .contains("not read-only"));

    select_board(
        root.path(),
        "local-test",
        board,
        "file-layout.yml",
        "capacity: 65536\nregions:\n  - name: resources\n    offset: 4096\n    size: 65536\n    access: read-only\n    filesystem: fatfs\n",
    );
    assert!(selected_resources_region(root.path())
        .expect_err("out-of-range resources region")
        .contains("exceeds"));
}

#[test]
fn accepts_both_supported_esp_resource_filesystems_but_requires_read_only() {
    let root = tempdir().expect("temporary workspace");
    let board = "name: esp-test\nhardware:\n  chip: esp32c6\ntoolchain:\n  target: riscv32imac-unknown-none-elf\nnative-layout:\n  artifact: partitions.csv\n";
    select_board(
        root.path(),
        "esp-test",
        board,
        "partitions.csv",
        "# Name, Type, SubType, Offset, Size, Flags\nfactory, app, factory, 0x10000, 0x100000,\nresources, data, littlefs, 0x3a0000, 0x60000, readonly\n",
    );
    let region = selected_resources_region(root.path()).expect("LittleFS resources partition");
    assert_eq!(region.filesystem(), ResourcesFilesystem::LittleFs);

    select_board(
        root.path(),
        "esp-test",
        board,
        "partitions.csv",
        "# Name, Type, SubType, Offset, Size, Flags\nfactory, app, factory, 0x10000, 0x100000,\nresources, data, fat, 0x3a0000, 0x60000,\n",
    );
    let error =
        selected_resources_region(root.path()).expect_err("writable FAT resources partition");
    assert!(error.contains("not read-only"), "unexpected error: {error}");
}

#[test]
fn rejects_invalid_stm32_resources_values() {
    let root = tempdir().expect("temporary workspace");
    select_board(
        root.path(),
        "stm-test",
        "name: stm-test\nhardware:\n  chip: stm32u5a5zj\ntoolchain:\n  target: thumbv8m.main-none-eabihf\nnative-layout:\n  artifact: memory.x\n",
        "memory.x",
        "MEMORY\n{\n  RESOURCES (r) : ORIGIN = nope, LENGTH = 256K /* filesystem: fatfs */\n}\n",
    );

    assert!(selected_resources_region(root.path())
        .expect_err("invalid STM32 origin")
        .contains("invalid STM32 layout value"));
}

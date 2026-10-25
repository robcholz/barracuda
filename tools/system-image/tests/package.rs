//! Selected-Board release packaging.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use barracuda_system_image::{
    firmware_selected, package_selected, FIRMWARE_OUTPUT, PACKAGE_OUTPUT,
};
use serde_json::Value;
use tempfile::tempdir;

const TARGET: &str = "test-none-elf";
const RESOURCES_OFFSET: usize = 64 * 1024;
const RESOURCES_SIZE: usize = 512 * 1024;

/// Selects Board `acme-board` on Platform `acme`, whose `application`
/// section is `application`, and writes a release build of `application`.
fn select_device_board(root: &Path, application: &str) {
    let bundle = root.join("boards/configs/acme-board");
    fs::create_dir_all(&bundle).expect("Board bundle");
    fs::create_dir_all(root.join(".barracuda")).expect("selection directory");
    fs::write(root.join(".barracuda/selected-board"), "acme-board\n").expect("selected Board");
    fs::write(
        bundle.join("board.yml"),
        format!(
            "name: acme-board\nhardware:\n  chip: acme1\ntoolchain:\n  target: {TARGET}\nnative-layout:\n  artifact: file-layout.yml\n"
        ),
    )
    .expect("Board definition");
    fs::write(
        bundle.join("file-layout.yml"),
        format!(
            "capacity: {}\nregions:\n  - name: resources\n    offset: {RESOURCES_OFFSET}\n    size: {RESOURCES_SIZE}\n    access: read-only\n    filesystem: fatfs\n",
            1024 * 1024
        ),
    )
    .expect("native layout");
    let platform = root.join("platforms/acme");
    fs::create_dir_all(&platform).expect("Platform directory");
    fs::write(
        platform.join("platform.yml"),
        format!(
            "name: acme\ninfo:\n  family: test\n  environment: bare-metal\npackage: test-platform-acme\ncrate: test_platform_acme\ntype: TestPlatform\nselection:\n  board-chips: [acme1]\n  targets:\n    - triple: {TARGET}\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .state\n    flash-image: physical.flash\napplication:\n{application}"
        ),
    )
    .expect("Platform definition");
    let release = root.join("target").join(TARGET).join("release");
    fs::create_dir_all(&release).expect("release directory");
    fs::write(release.join("barracuda-system"), b"application bytes").expect("release build");
}

fn manifest(directory: &Path) -> Value {
    serde_json::from_slice(&fs::read(directory.join("manifest.json")).expect("manifest"))
        .expect("manifest JSON")
}

#[test]
fn a_command_release_ships_its_raw_image_and_the_resources_at_their_offsets() {
    let root = tempdir().expect("temporary workspace");
    select_device_board(
        root.path(),
        "  release:\n    driver: command\n    program: cp\n    arguments: ['{application}', '{output}']\n    offset: 4096\n",
    );

    let package = package_selected(root.path(), "1.2.3").expect("package");

    let directory = root
        .path()
        .join(PACKAGE_OUTPUT)
        .join("barracuda-1.2.3-acme-board");
    assert_eq!(package.directory(), directory);
    assert_eq!(
        fs::read(directory.join("firmware.bin")).expect("firmware"),
        b"application bytes"
    );
    assert_eq!(
        fs::metadata(directory.join("resources.img"))
            .expect("resources")
            .len(),
        RESOURCES_SIZE as u64
    );
    let manifest = manifest(&directory);
    assert_eq!(manifest["version"], "1.2.3");
    assert_eq!(manifest["board"], "acme-board");
    assert_eq!(manifest["chip"], "acme1");
    assert_eq!(manifest["platform"], "acme");
    let parts = manifest["parts"].as_array().expect("parts");
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["path"], "firmware.bin");
    assert_eq!(parts[0]["format"], "raw");
    assert_eq!(parts[0]["offset"], 4096);
    assert_eq!(parts[0]["size"], 17);
    assert_eq!(
        parts[0]["sha256"],
        "bcf23e595c273e3202d8fc0fb47b5d6107a14fbc2d0a08c97ae4dd0262907d81"
    );
    assert_eq!(parts[1]["path"], "resources.img");
    assert_eq!(parts[1]["offset"], RESOURCES_OFFSET);
}

#[test]
fn an_elf_release_ships_the_build_without_an_offset() {
    let root = tempdir().expect("temporary workspace");
    select_device_board(root.path(), "  release:\n    driver: elf\n");

    let package = package_selected(root.path(), "1.2.3").expect("package");

    assert_eq!(package.parts()[0].path(), "firmware.elf");
    assert_eq!(package.parts()[0].offset(), None);
    let parts = manifest(package.directory())["parts"].clone();
    assert_eq!(parts[0]["format"], "elf");
    assert!(parts[0].get("offset").is_none());
    assert_eq!(parts[1]["offset"], RESOURCES_OFFSET);
}

#[test]
fn a_flash_size_template_needs_a_declared_size_outside_esp_layouts() {
    let root = tempdir().expect("temporary workspace");
    select_device_board(
        root.path(),
        "  release:\n    driver: command\n    program: cp\n    arguments: ['{application}', '{output}', '{flash-size}']\n    offset: 0\n",
    );

    let error = package_selected(root.path(), "1.2.3").expect_err("no flash size");

    assert!(error.contains("hardware.flash-size"), "{error}");
}

#[test]
fn a_platform_without_a_release_driver_publishes_nothing() {
    let root = tempdir().expect("temporary workspace");
    select_device_board(root.path(), "  support-binaries: []\n");

    let error = package_selected(root.path(), "1.2.3").expect_err("no release");

    assert!(error.contains("publishes no release firmware"), "{error}");
}

#[test]
fn a_missing_release_build_points_at_cargo_build() {
    let root = tempdir().expect("temporary workspace");
    select_device_board(root.path(), "  release:\n    driver: elf\n");
    fs::remove_file(
        root.path()
            .join("target")
            .join(TARGET)
            .join("release/barracuda-system"),
    )
    .expect("remove build");

    let error = package_selected(root.path(), "1.2.3").expect_err("no build");

    assert!(error.contains("cargo build --release"), "{error}");
}

#[test]
fn firmware_writes_the_release_firmware_without_the_resources() {
    let root = tempdir().expect("temporary workspace");
    select_device_board(
        root.path(),
        "  release:\n    driver: command\n    program: cp\n    arguments: ['{application}', '{output}']\n    offset: 4096\n",
    );

    let firmware = firmware_selected(root.path()).expect("firmware");

    let directory = root.path().join(FIRMWARE_OUTPUT).join("acme-board");
    assert_eq!(firmware.board(), "acme-board");
    assert_eq!(firmware.path(), directory.join("firmware.bin"));
    assert_eq!(firmware.offset(), Some(4096));
    assert_eq!(firmware.size(), 17);
    assert!(!directory.join("resources.img").exists());
}

#[test]
fn firmware_fails_when_the_release_driver_refuses_the_build() {
    let root = tempdir().expect("temporary workspace");
    // Stands in for espflash refusing an application larger than its slot.
    select_device_board(
        root.path(),
        "  release:\n    driver: command\n    program: 'false'\n    arguments: ['{application}', '{output}']\n    offset: 0\n",
    );

    let error = firmware_selected(root.path()).expect_err("refused");

    assert!(error.contains("false"), "{error}");
}

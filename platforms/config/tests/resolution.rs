//! Shared Platform resolution behavior.

#![allow(clippy::expect_used)]

use barracuda_platform_config::{resolve_board_platform, resolve_platform, PlatformTarget};

#[test]
fn resolves_every_registered_cargo_target() {
    let targets = [
        ("aarch64-apple-darwin", "macos", "aarch64", "macos"),
        ("x86_64-unknown-linux-gnu", "linux", "x86_64", "linux"),
        ("xtensa-esp32-none-elf", "none", "xtensa", "esp32"),
        ("xtensa-esp32s2-none-elf", "none", "xtensa", "esp32s2"),
        ("xtensa-esp32s3-none-elf", "none", "xtensa", "esp32s3"),
        ("riscv32imc-unknown-none-elf", "none", "riscv32", "esp32c3"),
        ("riscv32imac-unknown-none-elf", "none", "riscv32", "esp32c6"),
        (
            "riscv32imafc-unknown-none-elf",
            "none",
            "riscv32",
            "esp32p4",
        ),
        ("thumbv7em-none-eabihf", "none", "arm", "stm32"),
    ];

    for (triple, os, arch, expected) in targets {
        let target = PlatformTarget::new(triple, os, arch);
        assert_eq!(
            resolve_platform(target, None).expect("registered target"),
            expected
        );
    }
}

#[test]
fn resolves_platform_from_the_selected_board_definition() {
    let boards = [
        ("macos", None, "macos"),
        ("linux", None, "linux"),
        ("esp32", Some("xtensa-esp32-none-elf"), "esp32"),
        ("esp32c6", Some("riscv32imac-unknown-none-elf"), "esp32c6"),
        ("stm32f429zi", Some("thumbv7em-none-eabihf"), "stm32"),
    ];

    for (chip, target, expected) in boards {
        assert_eq!(
            resolve_board_platform(chip, target).expect("compatible Board"),
            expected
        );
    }
}

#[test]
fn rejects_board_target_and_platform_overrides_that_are_incompatible() {
    let error = resolve_board_platform("esp32c6", Some("riscv32imafc-unknown-none-elf"))
        .expect_err("Board target mismatch");
    assert!(error.to_string().contains("esp32c6"));
    assert!(error.to_string().contains("esp32p4"));

    let target = PlatformTarget::new("riscv32imac-unknown-none-elf", "none", "riscv32");
    let error = resolve_platform(target, Some("esp32p4")).expect_err("Platform override mismatch");
    assert!(error.to_string().contains("cannot be built"));
}

#[test]
fn rejects_unknown_targets_chips_and_platform_names() {
    let target = PlatformTarget::new("wasm32-unknown-unknown", "unknown", "wasm32");
    assert!(resolve_platform(target, None)
        .expect_err("unsupported target")
        .to_string()
        .contains("no Barracuda Platform"));
    assert!(resolve_board_platform("unknown-chip", None)
        .expect_err("unsupported Board chip")
        .to_string()
        .contains("unknown-chip"));

    let target = PlatformTarget::new("aarch64-apple-darwin", "macos", "aarch64");
    assert!(resolve_platform(target, Some("../macos"))
        .expect_err("unsafe Platform name")
        .to_string()
        .contains("invalid Platform name"));
}

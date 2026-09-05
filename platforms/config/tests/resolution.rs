//! Filesystem-discovered Platform resolution behavior.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use barracuda_platform_config::{resolve_board_platform, resolve_platform, PlatformTarget};
use tempfile::tempdir;

fn add_platform(root: &Path, name: &str, chip: &str, target: &str) {
    let directory = root.join("platforms").join(name);
    let crate_name = name.replace('-', "_");
    fs::create_dir_all(&directory).expect("Platform directory");
    fs::write(
        directory.join("platform.yml"),
        format!(
            "name: {name}\npackage: barracuda-platform-{name}\ncrate: barracuda_platform_{crate_name}\ntype: AcmePlatform\nselection:\n  board-chips:\n    - '{chip}'\n  targets:\n    - triple: '{target}'\nsystem-image:\n  layout:\n    driver: command\n    program: tools/system-image\n    arguments: [describe, '{{layout}}']\n  flash:\n    driver: command\n    program: tools/system-image\n    arguments: [flash, '{{layout}}', '{{image}}']\napplication:\n  support-binaries: [acme-network]\n  launcher:\n    program: privilege-tool\n    arguments: ['{{support:acme-network}}', '{{application}}']\n"
        ),
    )
    .expect("Platform manifest");
}

#[test]
fn discovers_an_unknown_platform_entirely_from_its_own_directory() {
    let root = tempdir().expect("temporary workspace");
    add_platform(
        root.path(),
        "acme-rv",
        "acme123*",
        "riscv64acme-unknown-none-elf",
    );

    let board = resolve_board_platform(
        root.path(),
        "acme123-pro",
        Some("riscv64acme-unknown-none-elf"),
    )
    .expect("Platform selected from Board");
    let target = resolve_platform(
        root.path(),
        PlatformTarget::new("riscv64acme-unknown-none-elf", "none", "riscv64"),
        None,
    )
    .expect("Platform selected from Cargo target");

    assert_eq!(board.name(), "acme-rv");
    assert_eq!(board.package(), "barracuda-platform-acme-rv");
    assert_eq!(target.name(), board.name());
    assert_eq!(board.directory(), root.path().join("platforms/acme-rv"));
    assert_eq!(board.application().support_binaries(), ["acme-network"]);
    assert_eq!(
        board
            .application()
            .launcher()
            .map(|launcher| launcher.program()),
        Some(Path::new("privilege-tool"))
    );
}

#[test]
fn rejects_missing_ambiguous_and_incompatible_platforms() {
    let root = tempdir().expect("temporary workspace");
    add_platform(root.path(), "first", "chip-*", "target-one");
    add_platform(root.path(), "second", "chip-*", "target-one");

    let error = resolve_board_platform(root.path(), "missing", None)
        .expect_err("missing Platform must fail");
    assert!(error.to_string().contains("no Platform"));

    let error = resolve_board_platform(root.path(), "chip-x", Some("target-one"))
        .expect_err("ambiguous Platform must fail");
    assert!(error.to_string().contains("multiple Platforms"));

    let target = PlatformTarget::new("target-one", "none", "custom");
    let error = resolve_platform(root.path(), target, Some("missing"))
        .expect_err("unknown requested Platform must fail");
    assert!(error.to_string().contains("missing"));
}

#[test]
fn central_resolver_contains_no_concrete_platform_registry() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let resolver = fs::read_to_string(root.join("platforms/config/src/lib.rs"))?;
    let selected = fs::read_to_string(root.join("platforms/selected/build.rs"))?;

    for concrete in [
        "macos", "linux", "esp32", "esp32c3", "esp32c6", "esp32p4", "esp32s2", "esp32s3", "stm32",
    ] {
        assert!(!resolver.contains(&format!("\"{concrete}\"")));
        assert!(!selected.contains(&format!("\"{concrete}\"")));
    }
    Ok(())
}

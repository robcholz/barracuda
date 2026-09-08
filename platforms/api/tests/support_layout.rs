//! Concrete Platform mechanisms belong to concrete Platform crates.

#![allow(clippy::expect_used)]

use std::{fs, path::Path};

#[test]
fn platform_mechanisms_are_not_standalone_pseudo_platforms() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    assert!(!root.join("shared/platform-std").exists());
    assert!(!root.join("shared/embassy-net-tun").exists());
    assert!(!root.join("platforms/file-storage").exists());
    assert!(!root.join("platforms/tun").exists());
    assert!(!root.join("platforms/net-gateway").exists());
    assert!(!root.join("platforms/net-gateway-protocol").exists());
    assert!(root
        .join("platforms/macos/network-gateway/Cargo.toml")
        .is_file());
    assert!(root.join("platforms/macos/platform.yml").is_file());
    assert!(root.join("platforms/linux/platform.yml").is_file());
    assert!(root.join("platforms/browser/platform.yml").is_file());
    assert!(root.join("platforms/esp32c6/platform.yml").is_file());
    for chip in ["esp32", "esp32s2", "esp32s3", "esp32c3", "esp32p4"] {
        assert!(root
            .join("platforms")
            .join(chip)
            .join("platform.yml")
            .is_file());
    }
}

#[test]
fn portable_runtime_does_not_branch_on_browser_targets() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for layer in ["composition", "core", "plugins", "shared"] {
        inspect_portable_sources(&root.join(layer));
    }
}

fn inspect_portable_sources(directory: &Path) {
    for entry in fs::read_dir(directory).expect("portable layer directory") {
        let path = entry.expect("portable layer entry").path();
        if path.is_dir() {
            inspect_portable_sources(&path);
            continue;
        }
        if !matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("rs" | "toml")
        ) || path.file_name().and_then(|name| name.to_str()) == Some("build.rs")
        {
            continue;
        }
        let source = fs::read_to_string(&path).expect("UTF-8 portable source");
        for forbidden in [
            "target_arch = \"wasm32\"",
            "CARGO_CFG_TARGET_ARCH",
            "wasm-static",
        ] {
            assert!(
                !source.contains(forbidden),
                "portable runtime source {} contains Browser target branch `{forbidden}`",
                path.display(),
            );
        }
    }
}

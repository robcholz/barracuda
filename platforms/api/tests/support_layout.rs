//! Concrete Platform mechanisms belong to concrete Platform crates.

use std::path::Path;

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
    assert!(root.join("platforms/esp32c6/platform.yml").is_file());
    for chip in ["esp32", "esp32s3", "esp32c3", "esp32p4"] {
        assert!(root
            .join("platforms")
            .join(chip)
            .join("platform.yml")
            .is_file());
    }
}

//! Platform selection is independent from Board selection.

#[test]
fn selected_platform_exports_no_board() -> Result<(), std::io::Error> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest_dir.join("src/lib.rs"))?;
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))?;
    assert!(!source.contains("BOARD"));
    assert!(!source.contains("SelectedBoardHal"));
    assert!(!manifest.contains("barracuda-board"));
    assert!(!manifest.contains("boards/"));
    Ok(())
}

#[test]
fn selected_native_platform_matches_the_target_operating_system() {
    assert_eq!(
        barracuda_platform_selected::PLATFORM_NAME,
        std::env::consts::OS
    );
}

#[test]
fn esp32c6_is_a_concrete_platform_identity() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = std::fs::read_to_string(root.join("platforms/selected/Cargo.toml"))?;
    let selection = std::fs::read_to_string(root.join("platforms/selected/build.rs"))?;
    let implementation = std::fs::read_to_string(root.join("platforms/esp32c6/src/lib.rs"))?;

    assert!(manifest.contains("barracuda-platform-esp32c6"));
    assert!(!manifest.contains("features = [\"esp32c6\"]"));
    assert!(!manifest.contains("cfg(target_arch = \"riscv32\")"));
    assert!(selection.contains("\"esp32c6\""));
    assert!(!implementation.contains("feature = \"esp32c6\""));
    Ok(())
}

#[test]
fn xtensa_esp_chips_have_concrete_platform_identities() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = std::fs::read_to_string(root.join("platforms/selected/Cargo.toml"))?;
    let selection = std::fs::read_to_string(root.join("platforms/selected/build.rs"))?;

    for chip in ["esp32", "esp32s2", "esp32s3"] {
        let implementation =
            std::fs::read_to_string(root.join("platforms").join(chip).join("src/lib.rs"))?;
        assert!(manifest.contains(&format!("barracuda-platform-{chip}")));
        assert!(selection.contains(&format!("\"{chip}\"")));
        assert!(!implementation.contains(&format!("feature = \"{chip}\"")));
    }
    Ok(())
}

#[test]
fn additional_riscv_esp_chips_have_concrete_platform_identities() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = std::fs::read_to_string(root.join("platforms/selected/Cargo.toml"))?;
    let selection = std::fs::read_to_string(root.join("platforms/selected/build.rs"))?;

    for chip in ["esp32c3", "esp32p4"] {
        let implementation =
            std::fs::read_to_string(root.join("platforms").join(chip).join("src/lib.rs"))?;
        assert!(manifest.contains(&format!("barracuda-platform-{chip}")));
        assert!(selection.contains(&format!("\"{chip}\"")));
        assert!(!implementation.contains(&format!("feature = \"{chip}\"")));
    }
    Ok(())
}

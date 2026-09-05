//! Board selection is independent from Platform selection.

#[test]
fn selected_board_exports_no_platform_identity() -> Result<(), std::io::Error> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest_dir.join("src/lib.rs"))?;
    let build = std::fs::read_to_string(manifest_dir.join("build.rs"))?;
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))?;
    assert!(!source.contains("SelectedPlatform"));
    assert!(!source.contains("PLATFORM_NAME"));
    assert!(!manifest.contains("barracuda-platform"));
    assert!(!manifest.contains("platforms/"));
    assert!(!build.contains("CARGO_CFG_TARGET_OS"));
    assert!(!build.contains("CARGO_CFG_TARGET_ARCH"));
    assert!(!build.contains("BARRACUDA_BOARD"));
    assert!(!build.contains("default_board("));
    assert!(!build.contains("selected_feature_board"));
    assert!(build.contains("read_selected_board"));
    assert!(build.contains("barracuda_selected_board_hal_implementation"));
    assert!(!build.contains("stm32f429zi-nucleo"));
    assert!(!manifest.contains("[features]"));
    Ok(())
}

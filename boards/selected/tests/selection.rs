//! Board selection is independent from Platform selection.

#[test]
fn selected_board_exports_no_platform_identity() -> Result<(), std::io::Error> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest_dir.join("src/lib.rs"))?;
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))?;
    assert!(!source.contains("SelectedPlatform"));
    assert!(!source.contains("PLATFORM_NAME"));
    assert!(!manifest.contains("barracuda-platform"));
    assert!(!manifest.contains("platforms/"));
    Ok(())
}

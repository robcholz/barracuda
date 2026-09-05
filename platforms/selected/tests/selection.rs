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
fn source_workspace_uses_only_the_non_runnable_placeholder() {
    assert_eq!(barracuda_platform_selected::PLATFORM_NAME, "unconfigured");
}

#[test]
fn selected_platform_uses_the_shared_host_resolver() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let selection = std::fs::read_to_string(root.join("platforms/selected/build.rs"))?;

    assert!(selection.contains("barracuda_platform_config::resolve_platform"));
    assert!(selection.contains("barracuda_selected_platform_implementation"));
    assert!(!selection.contains("fn default_platform"));
    assert!(!selection.contains("fn validate_target"));
    Ok(())
}

#[test]
fn selected_manifest_has_no_concrete_platform_dependencies() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = std::fs::read_to_string(root.join("platforms/selected/Cargo.toml"))?;
    for concrete in [
        "barracuda-platform-macos",
        "barracuda-platform-linux",
        "barracuda-platform-esp32",
        "barracuda-platform-stm32",
    ] {
        assert!(!manifest.contains(concrete));
    }
    Ok(())
}

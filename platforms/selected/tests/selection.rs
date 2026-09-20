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
fn source_workspace_uses_the_persisted_platform_selection() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let selected = std::fs::read_to_string(root.join(".barracuda/selected-platform"))?;
    assert_eq!(barracuda_platform_selected::PLATFORM_NAME, selected.trim());
    assert_eq!(
        barracuda_platform_selected::PLATFORM_INFO.name(),
        selected.trim()
    );
    assert!(!barracuda_platform_selected::PLATFORM_INFO
        .family()
        .is_empty());
    assert!(!barracuda_platform_selected::PLATFORM_INFO
        .architecture()
        .is_empty());
    assert!(!barracuda_platform_selected::PLATFORM_INFO
        .environment()
        .is_empty());
    Ok(())
}

#[test]
fn selected_platform_uses_only_its_persisted_axis() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let selection = std::fs::read_to_string(root.join("platforms/selected/build.rs"))?;

    assert!(selection.contains("barracuda_platform_config::discover_platforms"));
    assert!(selection.contains(".barracuda/selected-platform"));
    assert!(!selection.contains("selected-board"));
    assert!(!selection.contains("barracuda_board"));
    assert!(!selection.contains("fn default_platform"));
    assert!(!selection.contains("fn validate_target"));
    Ok(())
}

#[test]
fn platform_entry_is_forwarded_only_when_the_application_expands_it() -> Result<(), std::io::Error>
{
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let selection = std::fs::read_to_string(root.join("platforms/selected/build.rs"))?;

    assert!(selection.contains("macro_rules! platform_entry"));
    assert!(selection.contains("$crate::__platform::platform_entry!"));
    assert!(!selection.contains("pub use ::{}::platform_entry"));
    Ok(())
}

#[test]
fn selected_manifest_uses_the_local_dependency_boundary() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = std::fs::read_to_string(root.join("platforms/selected/Cargo.toml"))?;
    let selected = std::fs::read_to_string(root.join(".barracuda/selected-platform"))?;
    assert!(manifest.contains(
        "barracuda-platform-selection = { path = \"../../.barracuda/selection/platform\" }"
    ));
    assert!(!manifest.contains("# BEGIN GENERATED SELECTED PLATFORM"));

    let local = std::fs::read_to_string(root.join(".barracuda/selection/platform/Cargo.toml"))?;
    let expected_feature = format!("\"barracuda-platform-{}\"", selected.trim());
    let default = local
        .lines()
        .find(|line| line.starts_with("default = ["))
        .unwrap_or_default();
    assert!(default.contains(&expected_feature));
    Ok(())
}

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
fn selected_manifest_contains_only_the_generated_concrete_dependency() -> Result<(), std::io::Error>
{
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = std::fs::read_to_string(root.join("platforms/selected/Cargo.toml"))?;
    let selected = std::fs::read_to_string(root.join(".barracuda/selected-platform"))?;
    let expected_dependency = format!("barracuda-platform-{}.workspace = true", selected.trim());
    assert!(manifest.contains("# BEGIN GENERATED SELECTED PLATFORM"));
    assert!(manifest.contains("# END GENERATED SELECTED PLATFORM"));
    let generated = manifest
        .split_once("# BEGIN GENERATED SELECTED PLATFORM")
        .and_then(|(_before, generated)| {
            generated
                .split_once("# END GENERATED SELECTED PLATFORM")
                .map(|(generated, _after)| generated)
        })
        .unwrap_or_default();
    assert!(generated.contains(&expected_dependency));
    assert_eq!(
        generated
            .lines()
            .filter(|line| line.trim_start().starts_with("barracuda-platform-"))
            .count(),
        1
    );
    Ok(())
}

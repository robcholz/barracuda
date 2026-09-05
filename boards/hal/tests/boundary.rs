//! Board HAL resources remain separate from Platform resources.

#[test]
fn board_hal_api_does_not_own_platform_services() -> Result<(), std::io::Error> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest_dir.join("src/lib.rs"))?;
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))?;
    for forbidden in ["PlatformResources", "ip_stack", "partitions"] {
        assert!(
            !source.contains(forbidden),
            "Board HAL API contains Platform concept `{forbidden}`"
        );
    }
    assert!(!manifest.contains("barracuda-platform"));
    Ok(())
}

#[test]
fn peripheral_drivers_live_at_the_workspace_root() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    assert!(workspace.join("drivers/indicator-led/Cargo.toml").is_file());
    assert!(!workspace.join("boards/drivers").exists());
}

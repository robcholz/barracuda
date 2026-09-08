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

#[test]
fn peripheral_drivers_use_upstream_hal_contracts() -> Result<(), std::io::Error> {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    assert!(!workspace.join("platforms/hal/Cargo.toml").exists());
    for entry in std::fs::read_dir(workspace.join("drivers"))? {
        let path = entry?.path();
        if !path.join("driver.yml").is_file() {
            continue;
        }
        let manifest = std::fs::read_to_string(path.join("Cargo.toml"))?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?");
        assert!(
            manifest.contains("embedded-hal.workspace = true"),
            "Driver `{name}` must consume embedded-hal directly"
        );
        assert!(
            !manifest.contains("barracuda-hal"),
            "Driver `{name}` uses the removed Barracuda HAL facade"
        );
        for vendor in ["esp-hal", "embassy-stm32"] {
            assert!(
                !manifest.contains(vendor),
                "Driver `{name}` depends on vendor HAL `{vendor}`"
            );
        }
    }
    Ok(())
}

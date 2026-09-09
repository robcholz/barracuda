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
fn peripheral_implementations_are_grouped_by_api() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    assert!(workspace
        .join("peripherals/impl/indicator/indicator-led/Cargo.toml")
        .is_file());
    assert!(!workspace.join("boards/drivers").exists());
}

#[test]
fn peripheral_implementations_use_upstream_hal_contracts() -> Result<(), std::io::Error> {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    assert!(!workspace.join("platforms/hal/Cargo.toml").exists());
    for peripheral_entry in std::fs::read_dir(workspace.join("peripherals/impl"))? {
        let peripheral_path = peripheral_entry?.path();
        for implementation_entry in std::fs::read_dir(peripheral_path)? {
            let path = implementation_entry?.path();
            if !path.join("peripheral.yml").is_file() {
                continue;
            }
            let manifest = std::fs::read_to_string(path.join("Cargo.toml"))?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?");
            assert!(
                manifest.contains("embedded-hal.workspace = true")
                    || manifest.contains("barracuda-peripheral.workspace = true"),
                "peripheral implementation `{name}` must consume embedded-hal or the semantic API"
            );
            assert!(
                !manifest.contains("barracuda-hal"),
                "peripheral implementation `{name}` uses the removed Barracuda HAL facade"
            );
            for vendor in ["esp-hal", "embassy-stm32"] {
                assert!(
                    !manifest.contains(vendor),
                    "peripheral implementation `{name}` depends on vendor HAL `{vendor}`"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn chip_drivers_are_private_low_level_crates() -> Result<(), std::io::Error> {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    for entry in std::fs::read_dir(workspace.join("drivers/chips"))? {
        let path = entry?.path();
        if !path.join("Cargo.toml").is_file() {
            continue;
        }
        assert!(
            !path.join("peripheral.yml").exists(),
            "chip driver `{}` must not be catalog-discoverable",
            path.display()
        );
        let manifest = std::fs::read_to_string(path.join("Cargo.toml"))?;
        for forbidden in [
            "barracuda-peripheral",
            "barracuda-board",
            "barracuda-system",
            "barracuda-plugin",
            "barracuda-platform",
            "esp-hal",
            "embassy-stm32",
        ] {
            assert!(
                !manifest.contains(forbidden),
                "chip driver `{}` crosses the `{forbidden}` boundary",
                path.display()
            );
        }
    }
    Ok(())
}

#[test]
fn peripheral_implementations_do_not_reimplement_chip_drivers() -> Result<(), std::io::Error> {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    for peripheral_entry in std::fs::read_dir(workspace.join("peripherals/impl"))? {
        for implementation_entry in std::fs::read_dir(peripheral_entry?.path())? {
            let path = implementation_entry?.path();
            let source_path = path.join("src/lib.rs");
            if !path.join("peripheral.yml").is_file() || !source_path.is_file() {
                continue;
            }
            let source = std::fs::read_to_string(&source_path)?;
            let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
            let manifest = std::fs::read_to_string(path.join("Cargo.toml"))?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?");

            for forbidden in [
                ".write_read(",
                "fn read_register",
                "fn write_register",
                "INITIAL_REGISTERS",
                "QVGA_JPEG_REGISTERS",
                "fn encode_component",
                "RESET_BYTES",
            ] {
                assert!(
                    !production.contains(forbidden),
                    "peripheral implementation `{name}` contains chip-level pattern `{forbidden}`; move it to drivers/chips/<chip>"
                );
            }

            if production.contains("i2c::I2c") {
                assert!(
                    manifest.contains("barracuda-driver-") || manifest.contains("bmi2"),
                    "I2C peripheral implementation `{name}` must delegate chip behavior to a local or upstream chip driver"
                );
            }
            if production.contains("spi::SpiBus") {
                assert!(
                    manifest.contains("barracuda-driver-")
                        || manifest.contains("mipidsi")
                        || manifest.contains("epd-waveshare"),
                    "SPI peripheral implementation `{name}` must delegate chip behavior to a local or upstream chip driver"
                );
            }

            let assets = path.join("assets");
            if assets.is_dir() {
                for asset in std::fs::read_dir(assets)? {
                    let asset = asset?.path();
                    assert_ne!(
                        asset.extension().and_then(|extension| extension.to_str()),
                        Some("h"),
                        "peripheral implementation `{name}` owns a vendor register table `{}`; move it to its chip driver",
                        asset.display()
                    );
                }
            }
        }
    }
    Ok(())
}

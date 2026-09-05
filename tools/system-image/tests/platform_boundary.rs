//! Extensibility boundary for System image Platform support.

#[test]
fn system_image_core_has_no_concrete_platform_registry() -> Result<(), std::io::Error> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources = [
        std::fs::read_to_string(manifest.join("src/lib.rs"))?,
        std::fs::read_to_string(manifest.join("src/flash.rs"))?,
    ]
    .join("\n");

    for concrete in [
        "macos", "linux", "esp32", "esp32c3", "esp32c6", "esp32p4", "esp32s2", "esp32s3", "stm32",
    ] {
        assert!(!sources.contains(&format!("\"{concrete}\"")));
    }
    Ok(())
}

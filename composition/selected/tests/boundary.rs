//! The outer composition keeps Platform and Board HAL resources separate.

use barracuda_target::TargetResources;

#[test]
fn target_resources_do_not_flatten_the_two_axes() {
    let resources = TargetResources {
        platform: 1_u8,
        board_hal: 2_u16,
    };
    assert_eq!(resources.platform, 1);
    assert_eq!(resources.board_hal, 2);
}

#[test]
fn composition_source_does_not_parse_selection_yaml() -> Result<(), std::io::Error> {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )?;
    assert!(!source.contains("BARRACUDA_PLATFORM"));
    assert!(!source.contains("BARRACUDA_BOARD"));
    assert!(!source.contains("yaml"));
    Ok(())
}

#[test]
fn only_outer_composition_depends_on_both_selected_axes() -> Result<(), std::io::Error> {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )?;
    assert!(manifest.contains("barracuda-platform-selected"));
    assert!(manifest.contains("barracuda-board-selected"));
    Ok(())
}

#[test]
fn outer_composition_does_not_guess_a_board_from_the_target() -> Result<(), std::io::Error> {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )?;
    assert!(!manifest.contains("barracuda-board-selected/"));
    assert_eq!(manifest.matches("barracuda-board-selected").count(), 1);
    Ok(())
}

#[test]
fn shared_riscv_target_chip_selection_propagates_to_platform() -> Result<(), std::io::Error> {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )?;

    for chip in ["esp32c5", "esp32c6"] {
        let feature = manifest
            .lines()
            .find(|line| line.starts_with(&format!("{chip} = [")))
            .unwrap_or_else(|| panic!("missing {chip} target feature"));
        assert!(feature.contains(&format!("\"barracuda-platform-selected/{chip}\"")));
    }
    Ok(())
}

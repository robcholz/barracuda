//! Board selection is independent from Platform selection.

#[test]
fn selected_board_exports_its_fixed_identity() {
    assert_eq!(
        barracuda_board_selected::BOARD_INFO,
        barracuda_board_selected::BOARD.info()
    );
    assert_eq!(
        barracuda_board_selected::BOARD_INFO.name(),
        barracuda_board_selected::BOARD.name()
    );
    assert_eq!(
        barracuda_board_selected::BOARD_INFO.hardware().chip(),
        barracuda_board_selected::BOARD.hardware().chip()
    );
}

#[test]
fn selected_board_exports_no_platform_identity() -> Result<(), std::io::Error> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest_dir.join("src/lib.rs"))?;
    let build = std::fs::read_to_string(manifest_dir.join("build.rs"))?;
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))?;
    assert!(!source.contains("SelectedPlatform"));
    assert!(!source.contains("PLATFORM_NAME"));
    assert!(manifest.contains("barracuda-platform-selected.workspace = true"));
    assert!(!manifest.contains("barracuda-platform-esp"));
    assert!(!manifest.contains("platforms/"));
    assert!(!build.contains("CARGO_CFG_TARGET_OS"));
    assert!(!build.contains("CARGO_CFG_TARGET_ARCH"));
    assert!(!build.contains("BARRACUDA_BOARD"));
    assert!(!build.contains("default_board("));
    assert!(!build.contains("selected_feature_board"));
    assert!(build.contains("read_selected_board"));
    assert!(build.contains("load_catalog"));
    assert!(build.contains("resolve_board"));
    assert!(build.contains("render_board_hal"));
    assert!(build.contains("render_board_hal"));
    assert!(!build.contains("board.board_hal()"));
    assert!(!build.contains("nucleo-u5a5zj-q"));
    assert!(!manifest.contains("[features]"));
    assert!(manifest
        .contains("barracuda-board-selection = { path = \"../../.barracuda/selection/board\" }"));
    Ok(())
}

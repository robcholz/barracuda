//! Static Board descriptor contract tests.

use barracuda_board::{Board, Hardware, NativeLayout};

const BOARD: Board = Board::new(
    "test-board",
    Hardware::new("macos"),
    NativeLayout::new("file-layout.yml"),
);

#[test]
fn board_names_concrete_hardware_and_its_native_layout() {
    assert_eq!(BOARD.name(), "test-board");
    assert_eq!(BOARD.hardware().chip(), "macos");
    assert_eq!(BOARD.native_layout().artifact(), "file-layout.yml");
}

#[test]
fn board_api_has_no_system_storage_roles() -> Result<(), std::io::Error> {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )?;
    for forbidden in ["filesystem", "resources", "database", "plugin"] {
        assert!(
            !source.contains(forbidden),
            "Board API contains System-owned storage role `{forbidden}`"
        );
    }
    Ok(())
}

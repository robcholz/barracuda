//! Static Board descriptor contract tests.

use barracuda_board::{Board, Hardware, Storage};

const BOARD: Board = Board::new(
    "test-board",
    Hardware::new("host"),
    Storage::new("filesystem", Some("web-assets"), "database"),
);

#[test]
fn board_contains_only_platform_neutral_static_settings() {
    assert_eq!(BOARD.name(), "test-board");
    assert_eq!(BOARD.hardware().chip(), "host");
    assert_eq!(BOARD.storage().filesystem(), "filesystem");
    assert_eq!(BOARD.storage().web_assets(), Some("web-assets"));
    assert_eq!(BOARD.storage().database(), "database");
}

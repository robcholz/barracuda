//! Fixed Board identity behavior.

use barracuda_board::{Board, BoardInfo, Hardware, NativeLayout};

#[test]
fn board_info_is_a_fixed_view_of_the_board_identity() {
    let board = Board::new(
        "test-board",
        Hardware::new("test-chip"),
        NativeLayout::new("layout.yml"),
    );

    assert_eq!(
        board.info(),
        BoardInfo::new("test-board", Hardware::new("test-chip"))
    );
    assert_eq!(board.name(), board.info().name());
    assert_eq!(board.hardware(), board.info().hardware());
}

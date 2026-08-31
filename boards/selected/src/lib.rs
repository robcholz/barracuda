//! Independently selected Board matrix and Board HAL.

#![no_std]

use barracuda_board_hal::{BoardHal, BoardHalInitResult};
use embassy_executor::Spawner;

mod selected {
    include!(concat!(env!("OUT_DIR"), "/selected_board.rs"));
}

use selected::SelectedBoardHal;

/// Board selected by the Board build input.
pub use selected::BOARD;

/// Board HAL capability bundle for the selected Board.
pub type Resources = <SelectedBoardHal as BoardHal>::Resources;

/// Move-only chip bindings required by the selected Board HAL.
pub type Bindings = <SelectedBoardHal as BoardHal>::Bindings;

/// Board HAL initialization error for the selected Board.
pub type Error = <SelectedBoardHal as BoardHal>::Error;

/// Constructs only the selected Board HAL resources.
///
/// # Errors
///
/// Returns the selected Board HAL's peripheral initialization error.
pub async fn resources(
    spawner: Spawner,
    bindings: Bindings,
) -> BoardHalInitResult<SelectedBoardHal> {
    SelectedBoardHal::initialize(spawner, bindings).await
}

//! Board HAL composition contract.
//!
//! A Board HAL owns peripheral Driver construction and returns semantic
//! capabilities. It does not construct or contain Platform resources.

#![no_std]

use core::{convert::Infallible, future::Future};

use barracuda_board::Board;
use embassy_executor::Spawner;

/// Result of initializing one statically selected [`BoardHal`].
pub type BoardHalInitResult<H> = Result<<H as BoardHal>::Resources, <H as BoardHal>::Error>;

/// Statically composed Board matrix and peripheral Drivers.
pub trait BoardHal: Sized + 'static {
    /// Semantic hardware capabilities exposed to System or Plugins.
    type Resources;
    /// Board HAL initialization failure.
    type Error;

    /// Initializes peripheral Drivers for one concrete Board matrix.
    fn initialize(
        spawner: Spawner,
        board: &'static Board,
    ) -> impl Future<Output = BoardHalInitResult<Self>>;
}

/// HAL for desktop Boards that declare no peripheral Drivers.
pub struct EmptyBoardHal;

/// Explicit absence of Board peripheral capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoBoardCapabilities;

impl BoardHal for EmptyBoardHal {
    type Resources = NoBoardCapabilities;
    type Error = Infallible;

    async fn initialize(_spawner: Spawner, _board: &'static Board) -> BoardHalInitResult<Self> {
        Ok(NoBoardCapabilities)
    }
}

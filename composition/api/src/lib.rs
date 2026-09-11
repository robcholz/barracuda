//! Target resource boundary with independent Platform and Board HAL ownership.

#![no_std]

pub use barracuda_board::{BoardInfo, Hardware};
pub use barracuda_platform::PlatformInfo;

/// Fixed identity of the independently selected Platform and Board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetIdentity {
    platform: PlatformInfo,
    board: BoardInfo,
}

impl TargetIdentity {
    /// Combines the two independently selected identity axes.
    #[must_use]
    pub const fn new(platform: PlatformInfo, board: BoardInfo) -> Self {
        Self { platform, board }
    }

    /// Returns the selected execution Platform identity.
    #[must_use]
    pub const fn platform(&self) -> &PlatformInfo {
        &self.platform
    }

    /// Returns the selected Board identity.
    #[must_use]
    pub const fn board(&self) -> &BoardInfo {
        &self.board
    }
}

/// Move-only resources acquired once and split between the selected axes.
pub struct TargetBindings<Platform, BoardHal> {
    /// Raw resources assigned to Platform initialization.
    pub platform: Platform,
    /// Raw resources assigned to Board HAL initialization.
    pub board_hal: BoardHal,
}

impl<Platform, BoardHal> TargetBindings<Platform, BoardHal> {
    /// Creates the complete binding set after Target acquires its singleton.
    #[must_use]
    pub const fn new(platform: Platform, board_hal: BoardHal) -> Self {
        Self {
            platform,
            board_hal,
        }
    }

    /// Splits the value into independently owned Platform and Board bindings.
    #[must_use]
    pub fn split(self) -> (Platform, BoardHal) {
        (self.platform, self.board_hal)
    }
}

/// Target resources with Platform and Board HAL ownership kept explicit.
pub struct TargetResources<Platform, BoardHal> {
    /// Resources owned and produced by the selected Platform.
    pub platform: Platform,
    /// Capabilities owned and produced by the selected Board HAL.
    pub board_hal: BoardHal,
}

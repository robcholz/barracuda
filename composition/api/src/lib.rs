//! Target resource boundary with independent Platform and Board HAL ownership.

#![no_std]

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

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

    /// Returns how many long-lived connections this Target holds: the
    /// Platform's budget for a Board with or without external memory.
    #[must_use]
    pub const fn long_lived_connections(&self) -> u8 {
        self.platform
            .long_lived_connections()
            .for_board(self.board.hardware().external_memory().is_some())
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

#[cfg(test)]
mod tests {
    use barracuda_board::{
        ExternalMemory, ExternalMemoryInterface, ExternalMemoryTechnology, Hardware,
    };
    use barracuda_platform::ConnectionBudget;

    use super::{BoardInfo, PlatformInfo, TargetIdentity};

    #[test]
    fn long_lived_connections_follow_the_board_external_memory() {
        let platform = PlatformInfo::new("chip", "family", "arch", "bare-metal")
            .with_long_lived_connections(ConnectionBudget::new(1, 3));
        let plain = Hardware::new("chip");
        let psram = plain.with_external_memory(ExternalMemory::new(
            ExternalMemoryTechnology::Psram,
            ExternalMemoryInterface::OctalSpi,
            8 << 20,
        ));

        let without = TargetIdentity::new(platform, BoardInfo::new("plain", plain));
        let with = TargetIdentity::new(platform, BoardInfo::new("psram", psram));
        assert_eq!(without.long_lived_connections(), 1);
        assert_eq!(with.long_lived_connections(), 3);

        let undeclared = PlatformInfo::new("chip", "family", "arch", "bare-metal");
        let none = TargetIdentity::new(undeclared, BoardInfo::new("psram", psram));
        assert_eq!(none.long_lived_connections(), 0);
    }
}

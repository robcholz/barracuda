//! Target resource boundary with independent Platform and Board HAL ownership.

#![no_std]

/// Target resources with Platform and Board HAL ownership kept explicit.
pub struct TargetResources<Platform, BoardHal> {
    /// Resources owned and produced by the selected Platform.
    pub platform: Platform,
    /// Capabilities owned and produced by the selected Board HAL.
    pub board_hal: BoardHal,
}

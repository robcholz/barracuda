//! Command-level Sitronix ST77916 QSPI panel driver data.

#![no_std]

/// QSPI opcode used for register commands and parameters.
pub const COMMAND_WRITE_OPCODE: u8 = 0x02;
/// QSPI opcode used for pixel-memory writes.
pub const COLOR_WRITE_OPCODE: u8 = 0x32;
/// DCS command that begins writing pixels into display memory.
pub const MEMORY_WRITE: u8 = 0x2c;
/// Native panel width in pixels.
pub const WIDTH: u16 = 360;
/// Native panel height in pixels.
pub const HEIGHT: u16 = 360;

/// One controller command and its required settling delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    /// Eight-bit DCS/DBI command.
    pub command: u8,
    /// Command parameters.
    pub data: &'static [u8],
    /// Delay required after the command completes.
    pub delay_ms: u16,
}

include!(concat!(env!("OUT_DIR"), "/panel_commands.rs"));

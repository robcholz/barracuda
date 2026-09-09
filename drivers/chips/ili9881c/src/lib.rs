//! Command-level ILI9881C MIPI-DSI panel driver data.

#![no_std]

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

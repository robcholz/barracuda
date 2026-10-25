//! Host side of Espressif's ESP-Hosted-MCU protocol over SDIO.
//!
//! An ESP32 co-processor running ESP-Hosted-MCU's slave firmware gives a
//! radio-less host Wi-Fi: the host drives `esp_wifi_*` on the co-processor
//! through protobuf RPC messages and exchanges Ethernet frames with it, both
//! framed in ESP-Hosted packets on one SDIO link. This crate implements that
//! host side on any [`sdio::MmcBus`]:
//!
//! - [`frame`]: the 12-byte payload header, checksums, and packet streams;
//! - [`init`]: the start-up handshake and the compatibility checks;
//! - [`serial`] and [`rpc`]: the RPC framing and the messages Wi-Fi needs;
//! - [`transport`]: the ESP SDIO slave registers, credits, and FIFO;
//! - [`Runner`], [`Control`], and [`Devices`]: the task that owns the link,
//!   the Wi-Fi operations, and the station and access-point network devices
//!   for embassy-net.
//!
//! The wire formats follow ESP-Hosted-MCU's host driver and protobuf
//! definitions; the fields used are unchanged from 0.0.6 through 3.x. A
//! co-processor that announces an RPC dialect or SDIO layout this host does
//! not implement is refused at start-up with [`StartError::Incompatible`].

#![no_std]

extern crate alloc;

mod control;
pub mod frame;
pub mod init;
pub mod protobuf;
pub mod rpc;
mod runner;
pub mod serial;
pub mod transport;

use core::fmt;

pub use control::{Control, ControlError, MacAddresses};
pub use init::FirmwareVersion;
pub use runner::{Config, Devices, MTU, NetDevice, Runner, State, new};

/// What the co-processor announced when it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Started {
    /// Its chip (`ESP_PRIV_FIRMWARE_CHIP_*`).
    pub chip: u8,
    /// Its firmware version; ESP-Hosted announces one from 3.x on.
    pub firmware: Option<FirmwareVersion>,
    /// Whether its packets carry checksums.
    pub checksum: bool,
    /// Whether it aggregates SDIO transfers (ESP-Hosted 3.x).
    pub aggregated: bool,
}

/// The co-processor could not be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartError {
    /// The reset pin could not be driven.
    ResetPin,
    /// The SDIO card did not come up.
    Card(transport::TransportError),
    /// The co-processor sent no INIT event after the data path opened.
    NoInitEvent,
    /// The INIT event is malformed.
    Init(init::InitError),
    /// The co-processor runs firmware this host cannot drive.
    Incompatible(init::Incompatible),
    /// The co-processor sends aggregates larger than the host's buffer.
    AggregateTooLarge(u32),
}

impl fmt::Display for StartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResetPin => formatter.write_str("the co-processor reset pin failed"),
            Self::Card(error) => write!(formatter, "{error}"),
            Self::NoInitEvent => formatter.write_str(
                "the co-processor answered on SDIO but sent no ESP-Hosted INIT event; \
                 it may not run ESP-Hosted-MCU slave firmware",
            ),
            Self::Init(error) => write!(formatter, "{error}"),
            Self::Incompatible(error) => write!(formatter, "{error}"),
            Self::AggregateTooLarge(bytes) => write!(
                formatter,
                "the co-processor sends {bytes}-byte SDIO aggregates; this host reads at most {}",
                runner::RX_TRANSFER
            ),
        }
    }
}

impl core::error::Error for StartError {}

#[cfg(test)]
mod tests;

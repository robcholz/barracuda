//! Per-channel owner book for IMessage Gateway channels.
//!
//! Each external channel keeps a list of at most [`MAX_OWNERS`] senders who
//! may command the device. Messages from anyone else are dropped and counted.
//! A new sender becomes an owner by sending the six-digit pairing code shown
//! in the portal (or `/start <code>`, which a Telegram deep link sends). Codes
//! are minted from the Platform's entropy source, expire after
//! [`PAIRING_CODE_LIFETIME`], are compared in constant time, and rotate when
//! used or on request.

mod book;
mod code;
mod store;

#[cfg(test)]
mod store_tests;

pub use book::{Classification, Owner, PairingView};
pub(crate) use code::PairingEntropy;
#[cfg(test)]
pub(crate) use store::OWNERS_STORAGE_KEY;
pub use store::{Owners, OwnersError};

/// Reply sent once to a sender that just paired, in both portal languages.
pub const PAIRED_REPLY: &str = "Paired. You can start chatting.";

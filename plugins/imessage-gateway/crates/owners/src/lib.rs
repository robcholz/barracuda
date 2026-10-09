//! Per-channel owner book for IMessage Gateway channels.
//!
//! Each external channel keeps a list of at most [`MAX_OWNERS`] senders who
//! may command the device. Messages from anyone else are dropped and counted.
//! A new sender becomes an owner by sending the six-digit pairing code shown
//! in the portal (or `/start <code>`, which a Telegram deep link sends). Codes
//! are minted from the Platform's entropy source, expire after
//! [`PAIRING_CODE_LIFETIME`], are compared in constant time, and rotate when
//! used or on request.

#![no_std]

extern crate alloc;

mod book;
mod code;
mod store;

pub use book::{
    Classification, Owner, OwnerBook, PairingView, MAX_OWNERS, MAX_PAIRING_ATTEMPTS, OWNER_ID_MAX,
    OWNER_LABEL_MAX, PAIRING_CODE_LIFETIME,
};
pub use code::{PairingCode, PairingEntropy, PAIRING_CODE_DIGITS};
pub use store::{Owners, OwnersError, OWNERS_STORAGE_KEY};

/// Reply sent once to a sender that just paired, in both portal languages.
pub const PAIRED_REPLY: &str = "已绑定，可以开始对话了\nPaired. You can start chatting.";

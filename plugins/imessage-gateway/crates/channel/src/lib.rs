//! Shared plumbing for IMessage Gateway channel Plugins.
//!
//! Every external channel (Telegram, WeChat, QQ, BlueBubbles, Inkbox) uses the
//! same pieces:
//!
//! - [`ChannelMode`] and its storage ([`load_mode`], [`store_mode`]);
//! - the owner book ([`Owners`], re-exported from
//!   `barracuda-imessage-gateway-owners`);
//! - the receive-loop scaffold ([`ReceiveControl`], [`receive_runtime`]);
//! - the HTTP surface ([`status_response`], [`ModeEndpoint`],
//!   [`OwnersEndpoint`]) over [`ChannelControl`];
//! - the portal status mapping ([`entry_status`]).
//!
//! It lives apart from the base `imessage-gateway` Plugin crate so the
//! Gateway itself does not depend on the webserver or the captive portal; only
//! channel Plugins, which already depend on both, use it.

#![no_std]

extern crate alloc;

mod http;
mod mode;
mod receive;
mod status;

pub use barracuda_imessage_gateway_owners as owners;
pub use barracuda_imessage_gateway_owners::{
    Classification, Owner, Owners, OwnersError, PairingEntropy, PAIRED_REPLY,
};
pub use http::{
    status_response, sync_receive, ChannelControl, ModeEndpoint, ModeError, ModeFuture,
    OwnersEndpoint, JSON_CONTENT_TYPE,
};
pub use mode::{load_mode, store_mode, ChannelMode, MODE_STORAGE_KEY};
pub use receive::{
    receive_runtime, NoSlot, ReceiveChannel, ReceiveControl, ReceiveError, ReceiveFuture,
    ReceiveRuntime, ReceiveSession, ReceiveSlotSource, ReceiveState, ReceiveTiming, UnlimitedSlots,
};
pub use status::{channel_entry_status, entry_status};

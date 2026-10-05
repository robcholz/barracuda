//! Chat messages and text shared as compact JSON in bulk memory.
//!
//! A [`ChatMessage`] is one chat message object stored exactly as `serde_json`
//! encodes it, in a single bulk allocation shared by reference count. The
//! transcript persists those bytes verbatim, context assembly passes the same
//! handles along, and request bodies copy them unchanged, so a message exists
//! once in memory however many places hold it. [`BulkText`] is the same
//! storage for other large immutable strings such as rendered prompts.
//!
//! [`json`] holds the encoder that writes `serde_json`-identical output into
//! exactly sized bulk buffers.

#![no_std]

extern crate alloc;

pub mod json;
mod message;

pub use barracuda_bulk_memory::BulkText;
pub use message::{ChatMessage, ChatMessageError};

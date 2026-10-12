//! Minimal WebSocket client (RFC 6455) for `no_std` targets.
//!
//! It runs over any byte stream split into an [`embedded_io_async::Read`]
//! half and an [`embedded_io_async::Write`] half, such as the halves of a
//! receive lease's stream in `http-client`. [`connect`] writes the HTTP/1.1
//! Upgrade request, checks the server's `101 Switching Protocols` answer and
//! its `Sec-WebSocket-Accept`, and returns a [`WsReader`] and a [`WsWriter`].
//!
//! - Client frames are masked with a fresh key from a [`MaskSource`].
//! - Text, binary, ping, pong, close, and fragmented (continuation) messages
//!   are supported; extensions and subprotocols are not.
//! - A message larger than [`Limits::max_message`] (64 KiB by default) fails
//!   with [`WsError::TooLarge`] before its payload is buffered.
//! - [`WsReader::next`] is cancel-safe when the underlying read is, so a
//!   heartbeat timer can race it.
//!
//! Ping frames are returned to the caller, who answers with
//! [`WsWriter::pong`]; the halves stay independent so neither borrows the
//! other.

#![no_std]

extern crate alloc;

mod error;
mod frame;
mod handshake;
mod reader;
mod writer;

pub use error::{HandshakeError, WsError};
pub use frame::{CloseFrame, Message};
pub use handshake::{accept_key, connect, encode_key, Request, Url};
pub use reader::{Limits, WsReader};
pub use writer::{MaskSource, WsWriter};

/// Close code for a normal closure (RFC 6455 section 7.4.1).
pub const CLOSE_NORMAL: u16 = 1000;
/// Close code for a protocol error.
pub const CLOSE_PROTOCOL_ERROR: u16 = 1002;
/// Close code for a message too big to process.
pub const CLOSE_TOO_BIG: u16 = 1009;

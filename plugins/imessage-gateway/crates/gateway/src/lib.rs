//! Minimal channel registry and outbound messaging facade.
#![no_std]

extern crate alloc;

mod channel;
mod error;
mod facade;
mod model;

pub use channel::{ChannelFuture, MessageChannel};
pub use error::{ChannelError, GatewayError, StreamError};
pub use facade::{MessageChannelRegistration, MessageGateway};
pub use model::{
    BinaryBody, BinaryChunk, BinaryStream, DeleteMessageRequest, EditMessageRequest, JsonContent,
    MediaKind, MessageKind, MessageTarget, Operation, ReactRequest, SendMediaRequest,
    SendMessageRequest, SendReceipt, SendStream, SendStreamEvent, SendStreamRequest,
    SetTypingRequest, TextBody, TextChunk, TextStream,
};

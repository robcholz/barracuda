//! Minimal channel registry and outbound messaging facade.
#![no_std]

extern crate alloc;

mod channel;
mod error;
mod facade;
mod model;

pub use channel::{ChannelFuture, MessageChannel};
pub use error::{ChannelError, GatewayError, StreamError};
pub use facade::MessageGateway;
pub use model::{
    BinaryBody, BinaryStream, DeleteMessageRequest, EditMessageRequest, MediaKind, MessageTarget,
    Operation, ReactRequest, SendMediaRequest, SendMessageRequest, SendReceipt, SetTypingRequest,
    TextBody, TextStream,
};

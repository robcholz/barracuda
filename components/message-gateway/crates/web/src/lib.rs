//! Runtime-neutral Web channel and REST/SSE protocol model.
#![no_std]

extern crate alloc;

mod channel;
mod inbound;
mod model;
mod sse;

pub use channel::{SubscribeError, Web, WebSubscription};
pub use inbound::{
    InboundError, InboundFuture, InboundMedia, InboundMessage, InboundMessageSink, InboundReceipt,
    MessageBody, WebService,
};
pub use model::{MediaPhase, WebDelivery, WebEvent, WebEventData};
pub use sse::SseError;

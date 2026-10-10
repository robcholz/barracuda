//! Runtime-neutral Web channel and REST/SSE protocol model, plus the optional
//! WebSocket callback used when the IMessage Web Plugin registers its endpoint.
#![no_std]

extern crate alloc;

mod channel;
mod client_frame;
mod inbound;
mod model;
#[cfg(feature = "server")]
mod server;
mod sse;

pub use channel::{SubscribeError, Web, WebSubscription};
pub use client_frame::{WebClientControl, WebClientFrame, WebControl};
pub use inbound::{
    InboundControl, InboundError, InboundFuture, InboundMedia, InboundMessage, InboundMessageSink,
    InboundReceipt, MessageBody, WebService,
};
pub use model::{MediaPhase, WebDelivery, WebEvent, WebEventData};
#[cfg(feature = "server")]
pub use server::WebBridge;
pub use sse::SseError;

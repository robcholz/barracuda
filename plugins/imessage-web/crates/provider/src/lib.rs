//! Runtime-neutral Web channel and REST/SSE protocol model, plus the optional
//! WebSocket callback used when the IMessage Web Plugin registers its endpoint.
#![no_std]

extern crate alloc;

mod channel;
mod client_frame;
mod inbound;
mod media;
mod model;
#[cfg(feature = "server")]
mod server;
mod sse;

pub use channel::{SubscribeError, Web, WebSubscription};
pub use client_frame::{WebClientControl, WebClientFrame, WebControl};
pub use inbound::{
    InboundControl, InboundError, InboundFuture, InboundMessage, InboundMessageSink,
};
pub use media::{
    KeptMedia, MediaSpool, SpoolFuture, WebMedia, MAX_ATTACHMENTS, MAX_KEPT, MAX_UPLOADS,
    MEDIA_PATH, UNKNOWN_MIME,
};
pub use model::{Rejection, WebDelivery, WebEvent, WebEventData};
#[cfg(feature = "server")]
pub use server::WebBridge;
pub use sse::SseError;

#![no_std]

//! Typed Gateway operations and Workflow Event delivery.

extern crate alloc;

/// Normalized inbound Gateway event.
pub mod gateway_message_received;
/// Complete outbound message contract.
pub mod gateway_send;
/// Outbound media-stream contract.
pub mod gateway_send_media;
/// Outbound semantic-stream contract.
pub mod gateway_send_stream;
mod json;
/// Gateway provider routing identity.
pub mod route;
mod runtime;

pub use gateway_message_received::{GatewayInboundMessage, GatewayMessageReceived};
pub use gateway_send::{GatewaySendRequest, GatewaySendResponse};
pub use gateway_send_media::{GatewayMediaKind, GatewaySendMediaFinished, GatewaySendMediaRequest};
pub use gateway_send_stream::{GatewaySendStreamFinished, GatewaySendStreamRequest};
pub use json::{GatewayAccepted, GatewayOperationError};
pub use route::GatewayRoute;
pub use runtime::{GatewayIngress, GatewayIngressError, GatewayRuntime};

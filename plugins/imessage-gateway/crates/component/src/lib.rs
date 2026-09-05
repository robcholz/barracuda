#![no_std]

//! JSON Event ingress and Agent/Workflow JSON RPC adapters for Gateway.

extern crate alloc;

/// Component lifecycle and Gateway ingress.
pub mod component;
/// Normalized inbound Gateway event.
pub mod gateway_message_received;
/// Outbound Gateway send RPC.
pub mod gateway_send;
/// Outbound Gateway media-send RPC.
pub mod gateway_send_media;
/// Typed streaming outbound message RPC.
pub mod gateway_send_stream;
mod json;
/// Gateway provider routing identity.
pub mod route;

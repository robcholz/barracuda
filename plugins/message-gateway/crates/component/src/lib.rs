#![no_std]

//! Event Router lifecycle, Event ingress, and outbound RPC adapter for Gateway.

extern crate alloc;

/// Component lifecycle and Gateway ingress.
pub mod component;
/// Normalized inbound Gateway event.
pub mod gateway_message_received;
/// Outbound Gateway send RPC.
pub mod gateway_send;
/// Outbound Gateway media-send RPC.
pub mod gateway_send_media;
/// Gateway provider routing identity.
pub mod route;
/// Shared fixed-layout Gateway wire values.
pub mod wire;

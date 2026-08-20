#![no_std]

//! Event Router lifecycle, Event ingress, and outbound RPC adapter for Gateway.

extern crate alloc;

/// Component lifecycle and Gateway ingress.
pub mod component;
/// Normalized inbound Gateway event.
pub mod gateway_message_received;
/// Outbound Gateway send RPC.
pub mod gateway_send;
/// Gateway provider routing identity.
pub mod route;
/// Variable-length Gateway framing errors.
pub mod wire;

#![no_std]

//! Stateless RPC protocol conversion between Gateway and Agent Components.

extern crate alloc;

/// Agent event to Gateway send conversion RPC.
pub mod agent_to_gateway;
/// Gateway route to Agent session binding RPC.
pub mod bind_route;
/// Adapter lifecycle and shared correlation state.
pub mod component;
/// Gateway event to Agent append conversion RPC.
pub mod gateway_to_agent;

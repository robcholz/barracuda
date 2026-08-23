#![no_std]

//! Event Router Component bridging the Message Gateway and the Agent.
//!
//! This Component keeps the CLI (and any IM provider) fully decoupled from the
//! Agent: inbound gateway messages drive an Agent turn, and the turn's events
//! are mapped back onto ordinary IM messages delivered through `gateway.send`.
//! Nothing agent-specific crosses the gateway boundary; rich content is carried
//! as generic [`gateway::MessageKind`] roles.

extern crate alloc;

/// Bridge lifecycle: inbound `bridge.handle` RPC and the outbound event pump.
pub mod component;
/// Inbound `bridge.handle` RPC: append a gateway message to the Agent session.
pub mod handle;
/// Outbound mapping: Agent session events to `gateway.send` requests.
pub mod outbound;

pub use component::GatewayAgentBridge;

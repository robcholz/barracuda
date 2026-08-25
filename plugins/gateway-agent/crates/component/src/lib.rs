#![no_std]

//! Event Router Component bridging the IMessage Gateway and the Agent.
//!
//! This Component keeps the CLI (and any IM provider) fully decoupled from the
//! Agent: inbound gateway messages drive an Agent turn, and the turn's events
//! are mapped live onto `gateway.send_stream` primary and extra frames.

extern crate alloc;

/// Bridge lifecycle and two-step Workflow registration.
pub mod component;
/// Stateful `gateway_agent.respond` streaming mapper.
pub mod respond;

pub use component::GatewayAgentBridge;

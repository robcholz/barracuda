#![no_std]

//! Event Router Component exposing the existing Agent runtime API.

extern crate alloc;

/// Component lifecycle and RPC registration.
pub mod component;
/// `AgentRuntime::delete_session` RPC.
pub mod delete_session;
mod dto;
/// `AgentRuntime::link_api` RPC.
pub mod link_api;
/// `AgentRuntime::list_sessions` RPC.
pub mod list_sessions;
/// `AgentRuntime::new_session` RPC.
pub mod new_session;
/// `AgentRuntime::open_session` RPC.
pub mod open_session;
/// RPCs backed by an open `SessionControl`.
pub mod session;
/// Variable-length Agent RPC framing errors and helpers.
pub mod wire;

//! Lua execution as one bidirectional Event Router RPC.
//!
//! Each `vm.run` call creates one isolated, crate-configured Lua state, streams
//! `io.input()` messages into it, and streams captured `io.print(...)` messages back
//! to the caller.
#![no_std]

extern crate alloc;

/// Event Router lifecycle integration.
pub mod component;
/// The `vm.run` RPC contract and reusable handler.
pub mod run;

pub use component::{VmComponent, VmLimits};

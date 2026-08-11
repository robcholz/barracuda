#![no_std]

//! Platform adapters for the networking traits consumed by `claw-api`.
//!
//! HTTP, TLS, buffers, and connection reuse belong to `claw-api`. This crate
//! only bridges concrete platform stacks to `embedded-nal-async` and provides
//! deterministic test stacks.

extern crate alloc;
#[cfg(feature = "tokio")]
extern crate std;

#[cfg(feature = "testing")]
pub mod testing;
#[cfg(feature = "tokio")]
mod tokio_stack;
#[cfg(feature = "tokio")]
pub use tokio_stack::TokioStack;

pub use embedded_nal_async::{Dns, TcpConnect};

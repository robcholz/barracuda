#![no_std]

//! Platform adapters for the networking traits consumed by `barracuda-model-api`.
//!
//! HTTP, TLS, buffers, and connection reuse belong to `barracuda-model-api`. This crate
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

pub use embedded_nal_async::{AddrType, ConnectedUdp, Dns, TcpConnect, UdpStack, UnconnectedUdp};

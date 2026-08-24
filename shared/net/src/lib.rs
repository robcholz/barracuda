#![no_std]

//! Platform-neutral network contracts consumed by Barracuda.
//!
//! Concrete adapters belong to `platforms/<name>` crates. This crate only
//! exposes the protocol traits used by portable code.

pub use embedded_nal_async::{AddrType, ConnectedUdp, Dns, TcpConnect, UdpStack, UnconnectedUdp};

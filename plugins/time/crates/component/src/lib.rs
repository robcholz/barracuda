//! Network-synchronized real-time clock exposed through a dynamic typed RPC.
#![no_std]

extern crate alloc;

/// Event Router lifecycle and network-clock state.
pub mod component;
/// Typed `time.now` RPC contract.
pub mod now;
/// SNTP network time source.
pub mod sntp;

pub use component::{
    ClockState, SyncSample, TimeComponent, TimeConfig, TimeSource, TimeSourceError,
    TimeSourceFuture,
};

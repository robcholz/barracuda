//! Network-synchronized UTC clock capability and JSON RPC.
#![no_std]

extern crate alloc;

/// Event Router lifecycle and network-clock state.
pub mod component;
/// Typed `time.now` RPC contract.
pub mod now;
/// SNTP network time source.
pub mod sntp;

pub use component::{
    ClockError, SyncSample, TimeComponent, TimeConfig, TimeSource, TimeSourceError,
    TimeSourceFuture, UnixMillis, UtcClock, UtcClockUpdater, synchronize_clock, utc_clock,
};

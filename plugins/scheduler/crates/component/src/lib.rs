//! RTC-authoritative scheduling through JSON RPCs and JSON Events.
#![no_std]

extern crate alloc;

/// JSON `scheduler.cancel` RPC.
pub mod cancel;
/// Event Router lifecycle and RTC polling loop.
pub mod component;
/// JSON Event emitted for each committed occurrence.
pub mod event;
mod json;
mod model;
/// JSON `scheduler.schedule` RPC.
pub mod schedule;
mod state;

pub use component::{SchedulerComponent, SchedulerConfig, SchedulerStorageError};
pub use model::{ScheduleId, ScheduleIdError};

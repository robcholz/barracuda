//! RTC-authoritative scheduling through dynamic RPCs and typed Events.
#![no_std]

extern crate alloc;

/// Dynamic `scheduler.cancel` RPC.
pub mod cancel;
/// Event Router lifecycle and RTC polling loop.
pub mod component;
/// Event emitted for each committed occurrence.
pub mod event;
mod model;
/// Dynamic `scheduler.schedule` RPC.
pub mod schedule;
mod state;

pub use component::{SchedulerComponent, SchedulerConfig, SchedulerControl};
pub use model::{ScheduleId, ScheduleIdError};

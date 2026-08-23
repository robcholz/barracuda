use barracuda_event_router::{Event, Unary};
use getset::CopyGetters;
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::{ScheduleId, model::DueOccurrence};

/// Payload emitted for one accepted schedule occurrence.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
    CopyGetters,
)]
pub struct Triggered {
    /// Schedule that produced this occurrence.
    #[getset(get_copy = "pub")]
    id: ScheduleId,
    /// One-based committed run number.
    #[getset(get_copy = "pub")]
    run_number: u64,
}

impl Triggered {
    pub(crate) const fn new(occurrence: DueOccurrence) -> Self {
        Self {
            id: occurrence.id(),
            run_number: occurrence.run_number(),
        }
    }
}

/// Event emitted once for every committed schedule occurrence.
pub struct SchedulerTriggered;

impl Event for SchedulerTriggered {
    const ID: &'static str = "scheduler.triggered";
    type Message = Triggered;
    type Input = Unary;
}

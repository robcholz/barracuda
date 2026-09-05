use barracuda_event_router::Event;

/// Event emitted once for every committed schedule occurrence.
pub struct SchedulerTriggered;

impl Event for SchedulerTriggered {
    const ID: &'static str = "scheduler.triggered";
}

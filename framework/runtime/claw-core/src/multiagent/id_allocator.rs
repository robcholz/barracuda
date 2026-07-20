use claw_persistence::DurableState;

use crate::orchestrator::IdAllocators;
use crate::protocol::AgentId;

crate::define_id_allocator!(
    pub(crate) AgentIdAllocatorState(AgentId),
    AgentId(1)
);

/// Process-wide allocator shared by all multiagent runtimes.
#[derive(Clone)]
pub(crate) struct AgentIdAllocator(DurableState<IdAllocators>);

impl AgentIdAllocator {
    /// Process-local allocator used by isolated unit tests.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::from_state(DurableState::new(IdAllocators::default()))
    }

    pub(crate) fn from_state(state: DurableState<IdAllocators>) -> Self {
        Self(state)
    }

    pub(crate) fn next(&self) -> AgentId {
        self.0.get_mut().next_agent()
    }

    pub(crate) fn peek(&self) -> AgentId {
        self.0.get().next_agent_id()
    }
}

impl std::fmt::Debug for AgentIdAllocator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentIdAllocator")
            .field("next", &self.peek())
            .finish_non_exhaustive()
    }
}

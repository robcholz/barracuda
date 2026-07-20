use claw_persistence::DurableState;

use crate::protocol::AgentId;

crate::define_id_allocator!(
    pub(crate) AgentIdAllocatorState(AgentId),
    AgentId(1)
);

/// Process-wide allocator shared by all multiagent runtimes.
#[derive(Clone)]
pub(crate) struct AgentIdAllocator(DurableState<AgentIdAllocatorState>);

impl AgentIdAllocator {
    /// Process-local allocator used by isolated unit tests.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::from_state(AgentIdAllocatorState::default())
    }

    pub(crate) fn from_state(state: AgentIdAllocatorState) -> Self {
        Self(DurableState::new(state))
    }

    pub(crate) fn state(&self) -> &DurableState<AgentIdAllocatorState> {
        &self.0
    }

    pub(crate) fn next(&self) -> AgentId {
        self.0.get_mut().next()
    }

    pub(crate) fn peek(&self) -> AgentId {
        self.0.get().peek()
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

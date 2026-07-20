use claw_persistence::DurableState;

use crate::protocol::AgentId;
use crate::runtime_state::RuntimeState;

/// Process-wide allocator shared by all multiagent runtimes.
#[derive(Clone)]
pub(crate) struct AgentIdAllocator(DurableState<RuntimeState>);

impl AgentIdAllocator {
    /// Process-local allocator used by isolated unit tests.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self(DurableState::new(RuntimeState::default()))
    }

    pub(crate) fn from_runtime(runtime: DurableState<RuntimeState>) -> Self {
        Self(runtime)
    }

    pub(crate) fn next(&self) -> AgentId {
        let mut runtime = self.0.get_mut();
        let id = AgentId::new(runtime.next_agent_id());
        runtime.set_next_agent_id(id.0.saturating_add(1));
        id
    }

    pub(crate) fn peek(&self) -> AgentId {
        AgentId::new(self.0.get().next_agent_id())
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

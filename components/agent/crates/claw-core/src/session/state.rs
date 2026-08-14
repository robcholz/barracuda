//! Durable state owned by the Session subsystem.

use claw_permission::PermissionLevel;
use claw_persistence::DurableState;
use serde::{Deserialize, Serialize};

use crate::agent::{AgentId, AgentIdAllocator, ReasoningEffort};

use super::manager::SessionId;

crate::define_id_allocator!(
    /// Hands out process-unique session ids for the current runtime.
    pub(super) SessionIdAllocator(SessionId),
    SessionId(1)
);

#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct SessionManagerState {
    agent_id_allocator: AgentIdAllocator,
    session_id_allocator: SessionIdAllocator,
}

#[derive(Clone, Debug)]
pub(super) struct AgentIdAllocatorHandle {
    state: DurableState<SessionManagerState>,
}

impl AgentIdAllocatorHandle {
    pub(super) fn new(state: &DurableState<SessionManagerState>) -> Self {
        Self {
            state: state.clone(),
        }
    }

    pub(super) fn next(&self) -> AgentId {
        self.state.get_mut().agent_id_allocator.next()
    }
}

pub(super) fn allocate_session_id(state: &DurableState<SessionManagerState>) -> SessionId {
    state.get_mut().session_id_allocator.next()
}

pub(super) fn ensure_next_session_id(state: &DurableState<SessionManagerState>, next: SessionId) {
    let mut state = state.get_mut();
    if state.session_id_allocator.peek() < next {
        state.session_id_allocator = SessionIdAllocator::starting_at(next);
    }
}

pub(super) fn ensure_next_agent_id(state: &DurableState<SessionManagerState>, next: AgentId) {
    let mut state = state.get_mut();
    if state.agent_id_allocator.peek() < next {
        state.agent_id_allocator = AgentIdAllocator::starting_at(next);
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(super) struct SessionPersistentState {
    pub(super) reasoning_effort: ReasoningEffort,
    pub(super) permission_level: PermissionLevel,
    pub(super) root_agent: Option<AgentId>,
}

impl SessionPersistentState {
    pub(super) fn clear_root(&mut self) {
        self.root_agent = None;
    }
}

#[cfg(test)]
mod tests {
    use claw_persistence::DurableState;

    use super::{
        allocate_session_id, ensure_next_agent_id, ensure_next_session_id, AgentIdAllocatorHandle,
        SessionManagerState,
    };
    use crate::agent::AgentId;
    use crate::session::SessionId;

    #[test]
    fn manager_state_owns_both_global_allocators() {
        let state = DurableState::new(SessionManagerState::default());

        ensure_next_session_id(&state, SessionId::new(4));
        ensure_next_agent_id(&state, AgentId::new(7));

        assert_eq!(allocate_session_id(&state), SessionId::new(4));
        assert_eq!(AgentIdAllocatorHandle::new(&state).next(), AgentId::new(7));
    }
}

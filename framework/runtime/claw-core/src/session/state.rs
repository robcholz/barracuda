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
    agent_ids: AgentIdAllocator,
    session_ids: SessionIdAllocator,
}

pub(super) fn next_agent(state: &DurableState<SessionManagerState>) -> AgentId {
    state.get_mut().agent_ids.next()
}

pub(super) fn next_session(state: &DurableState<SessionManagerState>) -> SessionId {
    state.get_mut().session_ids.next()
}

pub(super) fn ensure_next_session(state: &DurableState<SessionManagerState>, next: SessionId) {
    let mut state = state.get_mut();
    if state.session_ids.peek() < next {
        state.session_ids = SessionIdAllocator::starting_at(next);
    }
}

pub(super) fn ensure_next_agent(state: &DurableState<SessionManagerState>, next: AgentId) {
    let mut state = state.get_mut();
    if state.agent_ids.peek() < next {
        state.agent_ids = AgentIdAllocator::starting_at(next);
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
        ensure_next_agent, ensure_next_session, next_agent, next_session, SessionManagerState,
    };
    use crate::agent::AgentId;
    use crate::session::SessionId;

    #[test]
    fn manager_state_owns_both_global_allocators() {
        let state = DurableState::new(SessionManagerState::default());

        ensure_next_session(&state, SessionId::new(4));
        ensure_next_agent(&state, AgentId::new(7));

        assert_eq!(next_session(&state), SessionId::new(4));
        assert_eq!(next_agent(&state), AgentId::new(7));
    }
}

//! Durable state owned by the Session subsystem.

use claw_api::ToolCall;
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
    /// Root-Agent calls that crossed the durable pre-execution boundary but
    /// have not yet reached a durably settled outcome.
    root_inflight_toolcalls: Vec<ToolCall>,
}

impl SessionPersistentState {
    pub(super) fn clear_root(&mut self) {
        self.root_agent = None;
        self.root_inflight_toolcalls.clear();
    }

    pub(super) fn root_inflight_toolcalls(&self) -> &[ToolCall] {
        &self.root_inflight_toolcalls
    }

    fn contains_root_inflight_toolcall(&self, call: &ToolCall) -> bool {
        self.root_inflight_toolcalls
            .iter()
            .any(|inflight| inflight == call)
    }

    pub(super) fn add_root_inflight_toolcall(&mut self, call: &ToolCall) {
        if self.contains_root_inflight_toolcall(call) {
            return;
        }
        self.root_inflight_toolcalls.push(call.clone());
    }

    pub(super) fn remove_root_inflight_toolcall(&mut self, call: &ToolCall) -> bool {
        if let Some(index) = self
            .root_inflight_toolcalls
            .iter()
            .position(|inflight| inflight == call)
        {
            self.root_inflight_toolcalls.remove(index);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use claw_api::ToolCall;
    use claw_persistence::DurableState;

    use super::{
        ensure_next_agent, ensure_next_session, next_agent, next_session, SessionManagerState,
        SessionPersistentState,
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

    #[test]
    fn root_inflight_toolcall_lifecycle_is_idempotent() {
        let mut state = SessionPersistentState::default();
        let call = ToolCall {
            id: "call-1".to_owned(),
            name: "profile_read".to_owned(),
            arguments_json: r#"{"document":"user"}"#.to_owned(),
        };

        state.add_root_inflight_toolcall(&call);
        state.add_root_inflight_toolcall(&call);
        assert!(state.contains_root_inflight_toolcall(&call));
        assert_eq!(state.root_inflight_toolcalls.len(), 1);

        assert!(state.remove_root_inflight_toolcall(&call));
        assert!(!state.contains_root_inflight_toolcall(&call));
    }
}

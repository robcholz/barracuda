//! Durable process-wide state owned by `SessionManager`.

use std::borrow::Cow;

use claw_persistence::{
    DurablePartError, DurableState, DurableStateCodec, SchemaVersion, StateBlob, StateSlice,
};
use serde::{Deserialize, Serialize};

use crate::agent::{AgentId, AgentIdAllocator};

pub(super) const SESSION_MANAGER_STATE_NAME: &str = "session_manager";

crate::define_prefixed_id!(SessionId, "session-", "session");
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

impl DurableStateCodec for SessionManagerState {
    const SCHEMA_VERSION: SchemaVersion = 1;

    fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
        Ok(StateBlob {
            bytes: Cow::Owned(serde_json::to_vec(self).map_err(DurablePartError::encode)?),
        })
    }

    fn decode_state(
        schema_version: SchemaVersion,
        state: StateSlice<'_>,
    ) -> Result<Self, DurablePartError> {
        if schema_version != Self::SCHEMA_VERSION {
            return Err(DurablePartError::InvalidState(
                "unsupported session manager state schema",
            ));
        }
        serde_json::from_slice(state.bytes).map_err(DurablePartError::decode)
    }
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

#[cfg(test)]
mod tests {
    use claw_persistence::{DurableState, DurableStateCodec, StateSlice};

    use super::{ensure_next_session, next_agent, next_session, SessionId, SessionManagerState};
    use crate::agent::AgentId;

    #[test]
    fn manager_state_owns_both_global_allocators() {
        let state = DurableState::new(SessionManagerState::default());

        ensure_next_session(&state, SessionId::new(4));

        assert_eq!(next_session(&state), SessionId::new(4));
        assert_eq!(next_agent(&state), AgentId::new(1));
    }

    #[test]
    fn manager_state_uses_named_fields() {
        let state = SessionManagerState::default();
        let encoded = state.encode_state().unwrap().into_owned();
        let json: serde_json::Value = serde_json::from_slice(&encoded.bytes).unwrap();

        assert_eq!(json["agent_ids"], "agent-1");
        assert_eq!(json["session_ids"], "session-1");

        SessionManagerState::decode_state(
            SessionManagerState::SCHEMA_VERSION,
            StateSlice {
                bytes: &encoded.bytes,
            },
        )
        .unwrap();
    }
}

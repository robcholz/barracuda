//! Persistence mapping for Session-owned state.

use std::borrow::Cow;

use claw_persistence::{
    DurablePartError, DurableStateCodec, InstanceId, SchemaVersion, StateBlob, StateSlice,
};

use super::manager::SessionId;
use super::state::{SessionManagerState, SessionPersistentState};

pub(super) const SESSION_MANAGER_STATE_NAME: &str = "session_manager";
pub(super) const SESSION_STATE_NAME: &str = "sessions";

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

impl DurableStateCodec for SessionPersistentState {
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
                "unsupported session state schema",
            ));
        }
        serde_json::from_slice(state.bytes).map_err(DurablePartError::decode)
    }
}

pub(super) fn session_instance(session: SessionId) -> InstanceId {
    InstanceId::new(session.to_wire()).expect("a SessionId wire value is a valid instance id")
}

#[cfg(test)]
mod tests {
    use claw_permission::PermissionLevel;
    use claw_persistence::{DurableStateCodec, StateSlice};

    use super::{SessionManagerState, SessionPersistentState};
    use crate::agent::{AgentId, ReasoningEffort};

    #[test]
    fn manager_state_uses_named_fields() {
        let state = SessionManagerState::default();
        let encoded = state.encode_state().unwrap().into_owned();
        let json: serde_json::Value = serde_json::from_slice(&encoded.bytes).unwrap();

        assert_eq!(json["agent_id_allocator"], "agent-1");
        assert_eq!(json["session_id_allocator"], "session-1");

        SessionManagerState::decode_state(
            SessionManagerState::SCHEMA_VERSION,
            StateSlice {
                bytes: &encoded.bytes,
            },
        )
        .unwrap();
    }

    #[test]
    fn session_payload_matches_the_documented_json_shape() {
        let state = SessionPersistentState {
            reasoning_effort: ReasoningEffort::Medium,
            permission_level: PermissionLevel::Ask,
            root_agent: Some(AgentId::new(7)),
        };

        let encoded = state.encode_state().unwrap().into_owned();
        let json: serde_json::Value = serde_json::from_slice(&encoded.bytes).unwrap();
        assert_eq!(json["reasoning_effort"], "medium");
        assert_eq!(json["permission_level"], "ask");
        assert_eq!(json["root_agent"], "agent-7");
        assert!(json.get("root_inflight_toolcalls").is_none());

        let restored = SessionPersistentState::decode_state(
            SessionPersistentState::SCHEMA_VERSION,
            StateSlice {
                bytes: &encoded.bytes,
            },
        )
        .unwrap();
        assert_eq!(restored, state);
    }
}

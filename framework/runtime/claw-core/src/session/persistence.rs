use std::borrow::Cow;

use claw_permission::PermissionLevel;
use claw_persistence::{
    DurablePartError, DurableStateCodec, InstanceId, SchemaVersion, StateBlob, StateSlice,
};
use serde::{Deserialize, Serialize};

use crate::agent::AgentState;
use crate::config::ReasoningEffort;
use crate::protocol::{InflightToolCall, SessionId};

pub(crate) const SESSION_STATE_NAME: &str = "sessions";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct SessionState {
    reasoning_effort: ReasoningEffort,
    permission_level: PermissionLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_state: Option<AgentState>,
    /// Compatibility journal until durable ToolCallId records move into
    /// AgentState. Physical tool runners and futures are never stored here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    inflight_toolcalls: Vec<InflightToolCall>,
}

#[derive(Clone)]
pub(crate) struct SessionRecovery {
    pub(crate) agent_state: AgentState,
    pub(crate) inflight_toolcalls: Vec<InflightToolCall>,
}

impl SessionState {
    pub(crate) fn reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
    }

    pub(crate) fn set_reasoning_effort(&mut self, reasoning_effort: ReasoningEffort) {
        self.reasoning_effort = reasoning_effort;
    }

    pub(crate) fn permission_level(&self) -> PermissionLevel {
        self.permission_level
    }

    pub(crate) fn set_permission_level(&mut self, permission_level: PermissionLevel) {
        self.permission_level = permission_level;
    }

    pub(crate) fn recovery(&self) -> Option<SessionRecovery> {
        let agent_state = self.agent_state.clone()?;
        Some(SessionRecovery {
            agent_state,
            inflight_toolcalls: self.inflight_toolcalls.clone(),
        })
    }

    pub(crate) fn record_recovery(&mut self, state: AgentState) {
        self.agent_state = Some(state);
    }

    pub(crate) fn recovery_matches(&self, state: &AgentState) -> bool {
        self.agent_state.as_ref() == Some(state)
    }

    fn contains_inflight_toolcall(&self, call: &InflightToolCall) -> bool {
        self.inflight_toolcalls
            .iter()
            .any(|inflight| inflight == call)
    }

    pub(crate) fn add_inflight_toolcall(&mut self, call: &InflightToolCall) {
        if self.contains_inflight_toolcall(call) {
            return;
        }
        self.inflight_toolcalls.push(call.clone());
    }

    pub(crate) fn remove_inflight_toolcall(&mut self, call: &InflightToolCall) -> bool {
        if let Some(index) = self
            .inflight_toolcalls
            .iter()
            .position(|inflight| inflight == call)
        {
            self.inflight_toolcalls.remove(index);
            true
        } else {
            false
        }
    }
}

impl DurableStateCodec for SessionState {
    const SCHEMA_VERSION: SchemaVersion = 3;

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

pub(crate) fn session_instance(session: SessionId) -> InstanceId {
    InstanceId::new(session.to_wire()).expect("a SessionId wire value is a valid instance id")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use claw_persistence::{DurableStateCodec, StateSlice};

    use super::SessionState;
    use crate::agent::AgentState;
    use crate::config::ReasoningEffort;
    use crate::protocol::InflightToolCall;
    use claw_permission::PermissionLevel;

    #[test]
    fn session_payload_matches_the_documented_json_shape() {
        let mut state = SessionState {
            reasoning_effort: ReasoningEffort::Medium,
            permission_level: PermissionLevel::Ask,
            ..SessionState::default()
        };
        state.record_recovery(AgentState::normal_for_test(
            vec!["tool_group_id".to_owned()],
        ));
        state.add_inflight_toolcall(&InflightToolCall::new(
            "subagent_spawn",
            json!({"kind":"worker","foreground":false}),
        ));

        let encoded = state.encode_state().unwrap().into_owned();
        let json: serde_json::Value = serde_json::from_slice(&encoded.bytes).unwrap();
        assert_eq!(json["reasoning_effort"], "medium");
        assert_eq!(json["permission_level"], "ask");
        assert_eq!(json["agent_state"]["agent_mode"], "normal");
        assert_eq!(
            json["agent_state"]["resumed"]["loaded_tool_groups"][0],
            "tool_group_id"
        );
        assert_eq!(json["inflight_toolcalls"][0]["tool"], "subagent_spawn");

        let restored = SessionState::decode_state(
            SessionState::SCHEMA_VERSION,
            StateSlice {
                bytes: &encoded.bytes,
            },
        )
        .unwrap();
        assert_eq!(restored, state);
    }

    #[test]
    fn inflight_toolcall_lifecycle_is_idempotent() {
        let mut state = SessionState::default();
        let call = InflightToolCall::new("profile_read", json!({"document":"user"}));

        state.add_inflight_toolcall(&call);
        state.add_inflight_toolcall(&call);
        assert!(state.contains_inflight_toolcall(&call));
        assert_eq!(state.inflight_toolcalls.len(), 1);

        assert!(state.remove_inflight_toolcall(&call));
        assert!(!state.contains_inflight_toolcall(&call));
    }
}

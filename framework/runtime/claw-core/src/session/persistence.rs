use std::borrow::Cow;

use claw_permission::PermissionLevel;
use claw_persistence::{
    DurablePartError, DurableStateCodec, Entry, InstanceId, SchemaVersion, StateBlob, StateSlice,
};
use serde::{Deserialize, Serialize};

use crate::agent::AgentMode;
use crate::config::ReasoningEffort;
use crate::protocol::{SessionId, TrackedToolCall};

const SESSION_NAMESPACE: &str = "sessions";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct SessionState {
    reasoning_effort: ReasoningEffort,
    permission_level: PermissionLevel,
    mode: AgentMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resume: Option<SessionResume>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
struct SessionResume {
    #[serde(default)]
    tool_set: ResumeToolSet,
    #[serde(default)]
    inflight_toolcalls: Vec<TrackedToolCall>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
struct ResumeToolSet {
    #[serde(default)]
    loaded_groups: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct SessionRecovery {
    pub(crate) loaded_tool_groups: Vec<String>,
    pub(crate) inflight_toolcalls: Vec<TrackedToolCall>,
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

    pub(crate) fn mode(&self) -> AgentMode {
        self.mode
    }

    pub(crate) fn recovery(&self) -> Option<SessionRecovery> {
        let resume = self.resume.as_ref()?;
        Some(SessionRecovery {
            loaded_tool_groups: resume.tool_set.loaded_groups.clone(),
            inflight_toolcalls: resume.inflight_toolcalls.clone(),
        })
    }

    pub(crate) fn record_recovery(&mut self, mode: AgentMode, mut loaded_groups: Vec<String>) {
        loaded_groups.sort_unstable();
        loaded_groups.dedup();
        self.mode = mode;

        let inflight_toolcalls = self
            .resume
            .take()
            .map(|resume| resume.inflight_toolcalls)
            .unwrap_or_default();
        self.resume = (!loaded_groups.is_empty() || !inflight_toolcalls.is_empty()).then_some(
            SessionResume {
                tool_set: ResumeToolSet { loaded_groups },
                inflight_toolcalls,
            },
        );
    }

    pub(crate) fn recovery_matches(&self, mode: AgentMode, loaded_groups: &[String]) -> bool {
        self.mode == mode
            && self
                .resume
                .as_ref()
                .map(|resume| resume.tool_set.loaded_groups.as_slice())
                .unwrap_or_default()
                == loaded_groups
    }

    fn contains_inflight_toolcall(&self, call: &TrackedToolCall) -> bool {
        self.resume.as_ref().is_some_and(|resume| {
            resume
                .inflight_toolcalls
                .iter()
                .any(|inflight| inflight == call)
        })
    }

    pub(crate) fn add_inflight_toolcall(&mut self, call: &TrackedToolCall) {
        if self.contains_inflight_toolcall(call) {
            return;
        }
        let resume = self.resume.get_or_insert_with(SessionResume::default);
        resume.inflight_toolcalls.push(call.clone());
    }

    pub(crate) fn remove_inflight_toolcall(&mut self, call: &TrackedToolCall) -> bool {
        let Some(resume) = self.resume.as_mut() else {
            return false;
        };
        let removed = if let Some(index) = resume
            .inflight_toolcalls
            .iter()
            .position(|inflight| inflight == call)
        {
            resume.inflight_toolcalls.remove(index);
            true
        } else {
            false
        };
        if resume.tool_set.loaded_groups.is_empty() && resume.inflight_toolcalls.is_empty() {
            self.resume = None;
        }
        removed
    }
}

impl DurableStateCodec for SessionState {
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

pub(crate) fn session_entry() -> Entry {
    Entry::collection(SESSION_NAMESPACE)
}

pub(crate) fn session_instance(session: SessionId) -> InstanceId {
    InstanceId::new(session.to_wire()).expect("a SessionId wire value is a valid instance id")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use claw_persistence::{DurableStateCodec, StateSlice};

    use super::SessionState;
    use crate::agent::AgentMode;
    use crate::config::ReasoningEffort;
    use crate::protocol::TrackedToolCall;
    use claw_permission::PermissionLevel;

    #[test]
    fn session_payload_matches_the_documented_json_shape() {
        let mut state = SessionState {
            reasoning_effort: ReasoningEffort::Medium,
            permission_level: PermissionLevel::Ask,
            mode: AgentMode::Normal,
            ..SessionState::default()
        };
        state.record_recovery(AgentMode::Normal, vec!["tool_group_id".to_owned()]);
        state.add_inflight_toolcall(&TrackedToolCall::new(
            "subagent_spawn",
            json!({"kind":"worker","foreground":false}),
        ));

        let encoded = state.encode_state().unwrap().into_owned();
        let json: serde_json::Value = serde_json::from_slice(&encoded.bytes).unwrap();
        assert_eq!(json["reasoning_effort"], "medium");
        assert_eq!(json["permission_level"], "ask");
        assert_eq!(json["mode"], "normal");
        assert_eq!(
            json["resume"]["tool_set"]["loaded_groups"][0],
            "tool_group_id"
        );
        assert_eq!(
            json["resume"]["inflight_toolcalls"][0]["tool"],
            "subagent_spawn"
        );

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
        let call = TrackedToolCall::new("profile_read", json!({"document":"user"}));

        state.add_inflight_toolcall(&call);
        state.add_inflight_toolcall(&call);
        assert!(state.contains_inflight_toolcall(&call));
        assert_eq!(
            state
                .resume
                .as_ref()
                .expect("resume exists")
                .inflight_toolcalls
                .len(),
            1
        );

        assert!(state.remove_inflight_toolcall(&call));
        assert!(!state.contains_inflight_toolcall(&call));
    }
}

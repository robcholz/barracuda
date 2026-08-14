//! Durable state owned by one BaseAgent and its stateful components.

use alloc::{
    borrow::{Cow, ToOwned},
    collections::{BTreeMap, BTreeSet},
    string::String,
    sync::Arc,
    vec::Vec,
};

use claw_api::ToolCall;
use claw_persistence::{
    DurablePartError, DurableState, DurableStateCodec, SchemaVersion, StateBlob, StateSlice,
};
use claw_tool::{AgentStorageBackend, AgentStorageError, AgentStorageScope, ToolGroupId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::context_providers::AgentMode;
use crate::agent::AgentKind;

/// Complete currently implemented BaseAgent recovery DTO.
///
/// Conversation history is not included: it is a projection of the canonical
/// transcript store. Runtime-only stream and poll state is not durable.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(in crate::agent) struct BaseAgentState {
    kind: String,
    mode: AgentMode,
    loaded_tool_groups: BTreeSet<String>,
    /// Calls checkpointed before execution and retained until their results
    /// have been recorded in the transcript.
    inflight_toolcalls: Vec<ToolCall>,
    #[serde(default)]
    toolcall_states: BTreeMap<ToolGroupId, ToolCallState>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct ToolCallState {
    object: Value,
}

impl BaseAgentState {
    pub(in crate::agent) fn new(kind: &AgentKind) -> Self {
        Self {
            kind: kind.as_str().to_owned(),
            mode: AgentMode::Normal,
            loaded_tool_groups: BTreeSet::new(),
            inflight_toolcalls: Vec::new(),
            toolcall_states: BTreeMap::new(),
        }
    }

    pub(crate) fn kind(&self) -> AgentKind {
        AgentKind::new(self.kind.clone())
    }

    pub(in crate::agent) fn mode(&self) -> AgentMode {
        self.mode
    }

    pub(in crate::agent) fn set_mode(&mut self, mode: AgentMode) {
        self.mode = mode;
    }

    pub(in crate::agent) fn loaded_tool_groups(&self) -> &BTreeSet<String> {
        &self.loaded_tool_groups
    }

    pub(in crate::agent) fn record_loaded_tool_group(&mut self, group_id: String) {
        self.loaded_tool_groups.insert(group_id);
    }

    pub(in crate::agent) fn inflight_toolcalls(&self) -> &[ToolCall] {
        &self.inflight_toolcalls
    }

    pub(in crate::agent) fn record_inflight_toolcalls(&mut self, calls: Vec<ToolCall>) {
        for call in calls {
            if !self.inflight_toolcalls.contains(&call) {
                self.inflight_toolcalls.push(call);
            }
        }
    }

    pub(in crate::agent) fn remove_inflight_toolcall(&mut self, id: &str) {
        self.inflight_toolcalls.retain(|call| call.id != id);
    }
}

pub(in crate::agent) fn tool_storage_scope(
    state: &DurableState<BaseAgentState>,
) -> AgentStorageScope {
    AgentStorageScope::new(Arc::new(AgentToolStorage {
        state: state.clone(),
    }))
}

struct AgentToolStorage {
    state: DurableState<BaseAgentState>,
}

impl AgentStorageBackend for AgentToolStorage {
    fn load(&self, group: &str) -> Result<Option<Value>, AgentStorageError> {
        Ok(self
            .state
            .get()
            .toolcall_states
            .get(group)
            .map(|state| state.object.clone()))
    }

    fn store(&self, group: &str, object: Value) -> Result<(), AgentStorageError> {
        self.state
            .get_mut()
            .toolcall_states
            .insert(group.into(), ToolCallState { object });
        Ok(())
    }

    fn clear(&self, group: &str) -> Result<(), AgentStorageError> {
        self.state.get_mut().toolcall_states.remove(group);
        Ok(())
    }
}

impl DurableStateCodec for BaseAgentState {
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
                "unsupported agent state schema",
            ));
        }
        serde_json::from_slice(state.bytes).map_err(DurablePartError::decode)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::{AgentToolStorage, BaseAgentState};
    use crate::agent::context_providers::AgentMode;
    use crate::agent::AgentKind;
    use claw_api::ToolCall;
    use claw_persistence::DurableState;
    use claw_persistence::{DurableStateCodec, StateSlice};
    use claw_tool::AgentStorageBackend;

    #[test]
    fn state_codec_round_trip_preserves_agent_state() {
        let mut state = BaseAgentState::new(&AgentKind::from_static("worker"));
        state.set_mode(AgentMode::Plan);
        state.record_loaded_tool_group("memory".to_owned());
        state.record_inflight_toolcalls(vec![ToolCall {
            id: "call-1".to_owned(),
            name: "profile_read".to_owned(),
            arguments_json: r#"{"document":"user"}"#.to_owned(),
        }]);

        let encoded = state.encode_state().expect("state encodes").into_owned();
        let json: serde_json::Value =
            serde_json::from_slice(&encoded.bytes).expect("state is JSON");
        assert_eq!(json["inflight_toolcalls"][0]["id"], "call-1");
        let decoded = BaseAgentState::decode_state(
            BaseAgentState::SCHEMA_VERSION,
            StateSlice {
                bytes: &encoded.bytes,
            },
        )
        .expect("state decodes");

        assert_eq!(decoded, state);
    }

    #[test]
    fn toolcall_state_is_stored_under_its_group_object() {
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        let storage = AgentToolStorage {
            state: state.clone(),
        };

        assert!(storage
            .store("todo", serde_json::json!({ "items": ["one"] }))
            .is_ok());

        let state = state.get();
        let encoded = state.encode_state().expect("state encodes");
        let encoded: serde_json::Value =
            serde_json::from_slice(&encoded.bytes).expect("state is JSON");
        assert_eq!(
            encoded.pointer("/toolcall_states/todo/object/items/0"),
            Some(&serde_json::Value::String("one".into()))
        );
    }
}

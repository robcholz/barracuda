//! Durable state owned by one BaseAgent.
//!
//! Context-provider state is opaque here and lives directly under
//! `context_provider_states.<context-provider-id>`.

use alloc::{
    borrow::{Cow, ToOwned},
    collections::BTreeMap,
    string::String,
    vec::Vec,
};

use barracuda_agent_persistence::{
    DurablePartError, DurableState, DurableStateCodec, SchemaVersion, StateBlob, StateSlice,
};
use barracuda_model_api::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::AgentKind;

/// Complete currently implemented BaseAgent recovery DTO.
///
/// Conversation history is not included: it is a projection of the canonical
/// transcript store. Runtime-only stream and poll state is not durable.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(in crate::agent) struct BaseAgentState {
    kind: String,
    /// Calls checkpointed before execution and retained until their results
    /// have been recorded in the transcript.
    inflight_toolcalls: Vec<ToolCall>,
    context_provider_states: BTreeMap<String, Value>,
}

impl BaseAgentState {
    pub(in crate::agent) fn new(kind: &AgentKind) -> Self {
        Self {
            kind: kind.as_str().to_owned(),
            inflight_toolcalls: Vec::new(),
            context_provider_states: BTreeMap::new(),
        }
    }

    pub(crate) fn kind(&self) -> AgentKind {
        AgentKind::new(self.kind.clone())
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

#[derive(Clone)]
pub(in crate::agent) struct AgentStorage {
    state: DurableState<BaseAgentState>,
    provider_id: &'static str,
}

impl AgentStorage {
    pub(in crate::agent) fn new(
        state: &DurableState<BaseAgentState>,
        provider_id: &'static str,
    ) -> Self {
        Self {
            state: state.clone(),
            provider_id,
        }
    }

    pub(in crate::agent) fn load(&self) -> Option<Value> {
        self.state
            .get()
            .context_provider_states
            .get(self.provider_id)
            .cloned()
    }

    pub(in crate::agent) fn store(&self, object: Value) {
        self.state
            .get_mut()
            .context_provider_states
            .insert(self.provider_id.into(), object);
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
    use super::{AgentStorage, BaseAgentState};
    use crate::agent::AgentKind;
    use barracuda_agent_persistence::DurableState;
    use barracuda_agent_persistence::{DurableStateCodec, StateSlice};
    use barracuda_model_api::ToolCall;

    #[test]
    fn state_codec_round_trip_preserves_agent_state() {
        let mut state = BaseAgentState::new(&AgentKind::from_static("worker"));
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
    fn context_provider_state_is_stored_directly_under_its_namespace() {
        let state = DurableState::new(BaseAgentState::new(&AgentKind::from_static("worker")));
        let storage = AgentStorage {
            state: state.clone(),
            provider_id: "todo",
        };

        storage.store(serde_json::json!({ "items": ["one"] }));

        let state = state.get();
        let encoded = state.encode_state().expect("state encodes");
        let encoded: serde_json::Value =
            serde_json::from_slice(&encoded.bytes).expect("state is JSON");
        assert_eq!(
            encoded.pointer("/context_provider_states/todo/items/0"),
            Some(&serde_json::Value::String("one".into()))
        );
    }
}

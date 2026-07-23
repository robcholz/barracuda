//! Complete typed DTO used to persist and restore one Agent.

use std::borrow::Cow;

use claw_persistence::{DurablePartError, DurableStateCodec, SchemaVersion, StateBlob, StateSlice};
use serde::{Deserialize, Serialize};

use crate::agent::context_adapters::{AgentModeState, ResumedState};
use crate::protocol::AgentKind;

/// Complete currently implemented Agent recovery DTO.
///
/// Conversation history is not included: it is a projection of the canonical
/// transcript store. Iteration-local tool-call IDs are never persisted here.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct AgentState {
    kind: String,
    agent_mode: AgentModeState,
    resumed: ResumedState,
}

impl AgentState {
    pub(in crate::agent) fn new(
        kind: &AgentKind,
        agent_mode: AgentModeState,
        resumed: ResumedState,
    ) -> Self {
        Self {
            kind: kind.as_str().to_owned(),
            agent_mode,
            resumed,
        }
    }

    pub(crate) fn kind(&self) -> AgentKind {
        AgentKind::new(self.kind.clone())
    }

    pub(in crate::agent) fn into_parts(self) -> (AgentModeState, ResumedState) {
        (self.agent_mode, self.resumed)
    }
}

/// Assembly sink used by authoritative Agent components.
pub(in crate::agent) struct AgentStateBuilder {
    kind: AgentKind,
    agent_mode: Option<AgentModeState>,
    resumed: Option<ResumedState>,
}

impl AgentStateBuilder {
    pub(super) fn new(kind: AgentKind) -> Self {
        Self {
            kind,
            agent_mode: None,
            resumed: None,
        }
    }

    pub(in crate::agent) fn set_resumed(&mut self, state: ResumedState) {
        debug_assert!(
            self.resumed.is_none(),
            "multiple adapters contributed ResumedState"
        );
        if self.resumed.is_none() {
            self.resumed = Some(state);
        }
    }

    pub(in crate::agent) fn set_agent_mode(&mut self, state: AgentModeState) {
        debug_assert!(
            self.agent_mode.is_none(),
            "multiple adapters contributed AgentModeState"
        );
        if self.agent_mode.is_none() {
            self.agent_mode = Some(state);
        }
    }

    pub(super) fn finish(self) -> AgentState {
        AgentState::new(
            &self.kind,
            self.agent_mode
                .expect("configured AgentModeContextAdapter must contribute AgentModeState"),
            self.resumed
                .expect("configured ResumedContextAdapter must contribute ResumedState"),
        )
    }
}

impl DurableStateCodec for AgentState {
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
mod tests {
    use super::AgentState;
    use crate::agent::context_adapters::{AgentModeState, ResumedState};
    use crate::protocol::AgentKind;
    use claw_persistence::{DurableStateCodec, StateSlice};

    #[test]
    fn state_codec_round_trip_preserves_typed_component_dtos() {
        let state = AgentState::new(
            &AgentKind::from_static("worker"),
            AgentModeState::Plan,
            ResumedState::new(vec!["memory".to_owned()]),
        );

        let encoded = state.encode_state().expect("state encodes").into_owned();
        let decoded = AgentState::decode_state(
            AgentState::SCHEMA_VERSION,
            StateSlice {
                bytes: &encoded.bytes,
            },
        )
        .expect("state decodes");

        assert_eq!(decoded, state);
    }
}

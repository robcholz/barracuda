use std::borrow::Cow;

use claw_interface::ClawFs;
use claw_persistence::{
    DurablePartError, DurableState, DurableStateCodec, Entry, PersistenceError, SchemaVersion,
    SharedPersistence, StateBlob, StateSlice,
};
use serde::{Deserialize, Serialize};

const RUNTIME_STATE_KEY: &str = "agent-system";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RuntimeState {
    next_session_id: u32,
    next_agent_id: u32,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            next_session_id: 1,
            next_agent_id: 1,
        }
    }
}

impl RuntimeState {
    pub(crate) fn next_session_id(&self) -> u32 {
        self.next_session_id
    }

    pub(crate) fn set_next_session_id(&mut self, next: u32) {
        self.next_session_id = next.max(1);
    }

    pub(crate) fn next_agent_id(&self) -> u32 {
        self.next_agent_id
    }

    pub(crate) fn set_next_agent_id(&mut self, next: u32) {
        self.next_agent_id = next.max(1);
    }

    fn normalize(&mut self) {
        self.next_session_id = self.next_session_id.max(1);
        self.next_agent_id = self.next_agent_id.max(1);
    }
}

impl DurableStateCodec for RuntimeState {
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
                "unsupported runtime state schema",
            ));
        }
        let mut decoded: Self =
            serde_json::from_slice(state.bytes).map_err(DurablePartError::decode)?;
        decoded.normalize();
        Ok(decoded)
    }
}

pub(crate) fn load_runtime_state<Filesystem: ClawFs>(
    persistence: &SharedPersistence<Filesystem>,
) -> Result<DurableState<RuntimeState>, PersistenceError> {
    let entry = Entry::singleton(RUNTIME_STATE_KEY);
    persistence.create_template::<RuntimeState>(entry.clone())?;
    match persistence.get::<RuntimeState>(&entry, None) {
        Ok(state) => Ok(state),
        Err(PersistenceError::StateNotFound { .. }) => {
            persistence.put(&entry, None, RuntimeState::default())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use claw_persistence::{DurableStateCodec, StateSlice};

    use super::RuntimeState;

    #[test]
    fn runtime_payload_matches_the_documented_json_shape() {
        let mut state = RuntimeState::default();
        state.set_next_session_id(4);
        state.set_next_agent_id(7);

        let encoded = state.encode_state().unwrap().into_owned();
        let json: serde_json::Value = serde_json::from_slice(&encoded.bytes).unwrap();
        assert_eq!(json["next_session_id"], 4);
        assert_eq!(json["next_agent_id"], 7);
        assert!(json.get("tool_registry").is_none());

        let restored = RuntimeState::decode_state(
            RuntimeState::SCHEMA_VERSION,
            StateSlice {
                bytes: &encoded.bytes,
            },
        )
        .unwrap();
        assert_eq!(restored.next_session_id(), 4);
        assert_eq!(restored.next_agent_id(), 7);
    }
}

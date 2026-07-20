use std::borrow::Cow;

use claw_interface::ClawFs;
use claw_persistence::{
    DurablePartError, DurableState, DurableStateCodec, PersistenceError, SchemaVersion,
    SharedPersistence, StateBlob, StateSlice,
};
use serde::{Deserialize, Serialize};

use crate::multiagent::AgentIdAllocatorState;
use crate::protocol::SessionIdAllocator;

const SESSION_ID_ALLOCATOR_ENTRY_NAME: &str = "session_id_allocator";
const AGENT_ID_ALLOCATOR_ENTRY_NAME: &str = "agent_id_allocator";
const LEGACY_ID_ALLOCATORS_ENTRY_NAME: &str = "id_allocators";

pub(super) struct LoadedIdAllocators {
    pub(super) session_ids: SessionIdAllocator,
    pub(super) agent_ids: AgentIdAllocatorState,
}

pub(super) fn load_id_allocators<Filesystem: ClawFs>(
    persistence: &SharedPersistence<Filesystem>,
) -> Result<LoadedIdAllocators, PersistenceError> {
    let session_ids = persistence
        .singleton::<SessionIdAllocator>(SESSION_ID_ALLOCATOR_ENTRY_NAME)?
        .load()?;
    let agent_ids = persistence
        .singleton::<AgentIdAllocatorState>(AGENT_ID_ALLOCATOR_ENTRY_NAME)?
        .load()?;

    match (session_ids, agent_ids) {
        (Some(session_ids), Some(agent_ids)) => Ok(LoadedIdAllocators {
            session_ids,
            agent_ids,
        }),
        (session_ids, agent_ids) => {
            let legacy = persistence
                .singleton::<LegacyIdAllocatorCheckpoint>(LEGACY_ID_ALLOCATORS_ENTRY_NAME)?
                .load()?
                .unwrap_or_default();
            let LoadedIdAllocators {
                session_ids: legacy_session_ids,
                agent_ids: legacy_agent_ids,
            } = legacy.into_allocators();
            Ok(LoadedIdAllocators {
                session_ids: session_ids.unwrap_or(legacy_session_ids),
                agent_ids: agent_ids.unwrap_or(legacy_agent_ids),
            })
        }
    }
}

pub(super) fn register_id_allocators<Filesystem: ClawFs>(
    persistence: &SharedPersistence<Filesystem>,
    session_ids: &DurableState<SessionIdAllocator>,
    agent_ids: &DurableState<AgentIdAllocatorState>,
) -> Result<(), PersistenceError> {
    persistence
        .singleton::<SessionIdAllocator>(SESSION_ID_ALLOCATOR_ENTRY_NAME)?
        .register(session_ids)?;
    persistence
        .singleton::<AgentIdAllocatorState>(AGENT_ID_ALLOCATOR_ENTRY_NAME)?
        .register(agent_ids)
}

#[derive(Debug, Deserialize, Serialize)]
struct SessionIdAllocatorDto {
    next_session_id: u32,
}

impl DurableStateCodec for SessionIdAllocator {
    const SCHEMA_VERSION: SchemaVersion = 1;

    fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
        let dto = SessionIdAllocatorDto {
            next_session_id: self.peek().0,
        };
        encode_json(&dto)
    }

    fn decode_state(
        schema_version: SchemaVersion,
        state: StateSlice<'_>,
    ) -> Result<Self, DurablePartError> {
        validate_schema(schema_version, "unsupported session id allocator schema")?;
        let dto: SessionIdAllocatorDto = decode_json(state)?;
        validate_counter(dto.next_session_id)?;
        Ok(Self::starting_at(dto.next_session_id.into()))
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct AgentIdAllocatorDto {
    next_agent_id: u32,
}

impl DurableStateCodec for AgentIdAllocatorState {
    const SCHEMA_VERSION: SchemaVersion = 1;

    fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
        let dto = AgentIdAllocatorDto {
            next_agent_id: self.peek().0,
        };
        encode_json(&dto)
    }

    fn decode_state(
        schema_version: SchemaVersion,
        state: StateSlice<'_>,
    ) -> Result<Self, DurablePartError> {
        validate_schema(schema_version, "unsupported agent id allocator schema")?;
        let dto: AgentIdAllocatorDto = decode_json(state)?;
        validate_counter(dto.next_agent_id)?;
        Ok(Self::starting_at(dto.next_agent_id.into()))
    }
}

/// Read-only migration shape used by the former combined singleton.
#[derive(Debug, Deserialize, Serialize)]
struct LegacyIdAllocatorCheckpoint {
    next_session_id: u32,
    next_agent_id: u32,
}

impl Default for LegacyIdAllocatorCheckpoint {
    fn default() -> Self {
        Self {
            next_session_id: 1,
            next_agent_id: 1,
        }
    }
}

impl LegacyIdAllocatorCheckpoint {
    fn into_allocators(self) -> LoadedIdAllocators {
        LoadedIdAllocators {
            session_ids: SessionIdAllocator::starting_at(self.next_session_id.into()),
            agent_ids: AgentIdAllocatorState::starting_at(self.next_agent_id.into()),
        }
    }
}

impl DurableStateCodec for LegacyIdAllocatorCheckpoint {
    const SCHEMA_VERSION: SchemaVersion = 1;

    fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
        encode_json(self)
    }

    fn decode_state(
        schema_version: SchemaVersion,
        state: StateSlice<'_>,
    ) -> Result<Self, DurablePartError> {
        validate_schema(schema_version, "unsupported id allocator checkpoint schema")?;
        let dto: Self = decode_json(state)?;
        validate_counter(dto.next_session_id)?;
        validate_counter(dto.next_agent_id)?;
        Ok(dto)
    }
}

fn encode_json(value: &impl Serialize) -> Result<StateBlob<'static>, DurablePartError> {
    Ok(StateBlob {
        bytes: Cow::Owned(serde_json::to_vec(value).map_err(DurablePartError::encode)?),
    })
}

fn decode_json<T: for<'de> Deserialize<'de>>(state: StateSlice<'_>) -> Result<T, DurablePartError> {
    serde_json::from_slice(state.bytes).map_err(DurablePartError::decode)
}

fn validate_schema(
    schema_version: SchemaVersion,
    unsupported: &'static str,
) -> Result<(), DurablePartError> {
    if schema_version == 1 {
        Ok(())
    } else {
        Err(DurablePartError::InvalidState(unsupported))
    }
}

fn validate_counter(counter: u32) -> Result<(), DurablePartError> {
    if counter == 0 {
        Err(DurablePartError::InvalidState(
            "id allocator counters must start at 1",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use claw_persistence::{DurableStateCodec, StateSlice};

    use super::{AgentIdAllocatorState, LegacyIdAllocatorCheckpoint, SessionIdAllocator};
    use crate::protocol::{AgentId, SessionId};

    #[test]
    fn narrow_allocator_states_round_trip_through_their_dtos() {
        let sessions = SessionIdAllocator::starting_at(SessionId::new(4));
        let session_bytes = sessions.encode_state().unwrap().into_owned();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&session_bytes.bytes).unwrap(),
            serde_json::json!({ "next_session_id": 4 })
        );
        assert_eq!(
            SessionIdAllocator::decode_state(
                SessionIdAllocator::SCHEMA_VERSION,
                StateSlice {
                    bytes: &session_bytes.bytes,
                },
            )
            .unwrap()
            .peek(),
            SessionId::new(4)
        );

        let agents = AgentIdAllocatorState::starting_at(AgentId::new(7));
        let agent_bytes = agents.encode_state().unwrap().into_owned();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&agent_bytes.bytes).unwrap(),
            serde_json::json!({ "next_agent_id": 7 })
        );
        assert_eq!(
            AgentIdAllocatorState::decode_state(
                AgentIdAllocatorState::SCHEMA_VERSION,
                StateSlice {
                    bytes: &agent_bytes.bytes,
                },
            )
            .unwrap()
            .peek(),
            AgentId::new(7)
        );
    }

    #[test]
    fn legacy_checkpoint_is_only_converted_at_load() {
        let loaded = LegacyIdAllocatorCheckpoint {
            next_session_id: 4,
            next_agent_id: 7,
        }
        .into_allocators();

        assert_eq!(loaded.session_ids.peek(), SessionId::new(4));
        assert_eq!(loaded.agent_ids.peek(), AgentId::new(7));
    }

    #[test]
    fn zero_allocator_start_is_rejected() {
        let error = SessionIdAllocator::decode_state(
            SessionIdAllocator::SCHEMA_VERSION,
            StateSlice {
                bytes: br#"{"next_session_id":0}"#,
            },
        )
        .expect_err("zero is not a valid allocator start");

        assert!(error.to_string().contains("must start at 1"));
    }
}

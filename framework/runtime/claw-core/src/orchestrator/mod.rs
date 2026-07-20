//! Process-wide facade and worker for session actors.
//!
//! The worker owns session lookup and cooperatively polls one actor per live
//! session. Turn state, agent graphs, and model-facing tools do not live here.

mod engine;
mod handle;

use std::borrow::Cow;
use std::io;

use claw_memory::LongTermInitError;
use claw_persistence::{
    DurablePartError, DurableStateCodec, PersistenceError, SchemaVersion, StateBlob, StateSlice,
};
use claw_skill::SkillError;
use serde::{Deserialize, Serialize};

use crate::agent::FsAgentFactoryError;
use crate::multiagent::AgentIdAllocatorState;
use crate::protocol::{AgentId, SessionId, SessionIdAllocator};
pub use handle::Orchestrator;

const ID_ALLOCATORS_STATE_NAME: &str = "id_allocators";

#[derive(Debug, Default)]
pub(crate) struct IdAllocators {
    sessions: SessionIdAllocator,
    agents: AgentIdAllocatorState,
}

impl IdAllocators {
    pub(crate) fn next_session(&mut self) -> SessionId {
        self.sessions.next()
    }

    pub(crate) fn ensure_next_session(&mut self, next: SessionId) {
        if self.sessions.peek() < next {
            self.sessions = SessionIdAllocator::starting_at(next);
        }
    }

    pub(crate) fn next_agent(&mut self) -> AgentId {
        self.agents.next()
    }

    pub(crate) fn next_agent_id(&self) -> AgentId {
        self.agents.peek()
    }
}

#[derive(Deserialize, Serialize)]
struct IdAllocatorsDto {
    next_session_id: u32,
    next_agent_id: u32,
}

impl DurableStateCodec for IdAllocators {
    const SCHEMA_VERSION: SchemaVersion = 1;

    fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
        let dto = IdAllocatorsDto {
            next_session_id: self.sessions.peek().0,
            next_agent_id: self.agents.peek().0,
        };
        Ok(StateBlob {
            bytes: Cow::Owned(serde_json::to_vec(&dto).map_err(DurablePartError::encode)?),
        })
    }

    fn decode_state(
        schema_version: SchemaVersion,
        state: StateSlice<'_>,
    ) -> Result<Self, DurablePartError> {
        if schema_version != Self::SCHEMA_VERSION {
            return Err(DurablePartError::InvalidState(
                "unsupported id allocator state schema",
            ));
        }
        let dto: IdAllocatorsDto =
            serde_json::from_slice(state.bytes).map_err(DurablePartError::decode)?;
        if dto.next_session_id == 0 || dto.next_agent_id == 0 {
            return Err(DurablePartError::InvalidState(
                "id allocator counters must start at 1",
            ));
        }
        Ok(Self {
            sessions: SessionIdAllocator::starting_at(dto.next_session_id.into()),
            agents: AgentIdAllocatorState::starting_at(dto.next_agent_id.into()),
        })
    }
}

/// What can go wrong while building an [`Orchestrator`].
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorBuildError {
    #[error("persistence directory is required")]
    MissingPersistenceDir,
    #[error("failed to load long-term memory: {0}")]
    LongTermInit(#[from] LongTermInitError),
    #[error("failed to load skill catalog: {0}")]
    SkillRegistry(#[from] SkillError),
    #[error("failed to initialize persistence: {0}")]
    Persistence(#[from] PersistenceError),
    #[error("invalid persisted session id: {0}")]
    InvalidSessionId(#[from] claw_utils::IdParseError),
    #[error("failed to spawn the orchestrator worker: {0}")]
    WorkerSpawn(#[from] io::Error),
    #[error("orchestrator worker exited before signalling readiness")]
    WorkerExitedBeforeReady,
}

impl From<FsAgentFactoryError> for OrchestratorBuildError {
    fn from(error: FsAgentFactoryError) -> Self {
        match error {
            FsAgentFactoryError::MissingPersistenceDir => Self::MissingPersistenceDir,
            FsAgentFactoryError::LongTermInit(source) => Self::LongTermInit(source),
            FsAgentFactoryError::SkillRegistry(source) => Self::SkillRegistry(source),
        }
    }
}

const ENGINE_WORKER_STACK_SIZE: usize = 64 * 1024;
const SYSTEM_TRACE_SCOPE: &str = "agent-system";
const ORCHESTRATOR_TRACE_TASK: &str = "orchestrator";

#[cfg(test)]
mod tests {
    use claw_persistence::{DurableStateCodec, StateSlice};

    use super::IdAllocators;
    use crate::protocol::{AgentId, SessionId};

    #[test]
    fn id_allocator_dto_constructs_both_runtime_allocators() {
        let mut allocators = IdAllocators::decode_state(
            IdAllocators::SCHEMA_VERSION,
            StateSlice {
                bytes: br#"{"next_session_id":4,"next_agent_id":7}"#,
            },
        )
        .unwrap();

        assert_eq!(allocators.next_session(), SessionId::new(4));
        assert_eq!(allocators.next_agent(), AgentId::new(7));
    }
}

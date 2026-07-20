//! Process-wide facade and worker for session actors.
//!
//! The worker owns session lookup and cooperatively polls one actor per live
//! session. Turn state, agent graphs, and model-facing tools do not live here.

mod engine;
mod handle;
mod id_allocators;

use std::io;

use claw_memory::LongTermInitError;
use claw_persistence::PersistenceError;
use claw_skill::SkillError;

use crate::agent::FsAgentFactoryError;
pub use handle::Orchestrator;

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

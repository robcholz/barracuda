use claw_memory::{
    LongTermInitError, TranscriptDeleteError, TranscriptInitError, TranscriptListError,
};
use claw_persistence::{InvalidInstanceId, PersistenceError};
use claw_skill::SkillError;
use claw_tool::ToolSetError;

use super::AgentId;

/// What can go wrong while building an [`super::AgentManager`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum AgentManagerError {
    /// No persistence directory was provided to the manager.
    #[error("persistence directory is required")]
    MissingPersistenceDir,
    /// A long-term memory journal exists but could not be read at startup.
    #[error("failed to load long-term memory: {0}")]
    LongTermInit(#[from] LongTermInitError),
    /// The configured skill catalog could not be scanned.
    #[error("failed to load skill catalog: {0}")]
    SkillRegistry(#[from] SkillError),
    /// Persisted Agent records and transcript files could not be reconciled.
    #[error("failed to reconcile persisted agent storage: {0}")]
    AgentReconciliation(#[from] AgentCreateError),
}

/// What can go wrong while building one concrete agent from the manager.
#[derive(Debug, thiserror::Error)]
pub enum AgentCreateError {
    /// An internally generated Agent id could not be represented as a persistence key.
    #[error(transparent)]
    InvalidInstanceId(#[from] InvalidInstanceId),
    /// The Agent recovery collection could not be accessed or registered.
    #[error("failed to access persisted agent state: {0}")]
    Persistence(#[from] PersistenceError),
    /// No persisted Agent record exists for the requested id.
    #[error("persisted agent not found: {0}")]
    AgentNotFound(AgentId),
    /// Creating a fresh persistent Agent would overwrite an existing record.
    #[error("persisted agent already exists: {0}")]
    AgentAlreadyExists(AgentId),
    /// An entry in the Agent collection did not use an AgentId wire key.
    #[error("invalid persisted agent id: {0}")]
    InvalidPersistedAgentId(String),
    /// No baked manifest exists for the requested kind.
    #[error("unknown agent kind: {0}")]
    UnknownKind(String),
    /// The agent's local tools could not be added to the tool set.
    #[error("failed to assemble agent tools: {0}")]
    Tools(#[from] ToolSetError),
    /// The transcript store for this placement could not be opened.
    #[error("failed to open transcript: {0}")]
    Transcript(#[from] TranscriptInitError),
    /// The persisted transcript could not be deleted.
    #[error("failed to delete transcript: {0}")]
    TranscriptDelete(#[from] TranscriptDeleteError),
    /// Persisted transcript ids could not be enumerated.
    #[error("failed to list persisted transcripts: {0}")]
    TranscriptList(#[from] TranscriptListError),
    /// The per-agent long-term memory store could not be opened.
    #[error("failed to load long-term memory: {0}")]
    LongTerm(#[from] LongTermInitError),
}

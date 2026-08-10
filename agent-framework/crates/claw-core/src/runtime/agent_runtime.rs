//! Executor-neutral process runtime ownership and configuration entry point.

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use async_channel::Sender;
use claw_api::{ClawApiConfig, InitError};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::LongTermInitError;
use claw_persistence::{PersistenceError, SharedPersistence};
use claw_skill::SkillError;
use claw_tool::ToolRegistry;

use crate::agent::AgentManagerError;
use crate::config::{ApiPurpose, SharedApiManager};
use crate::session::{
    OpenSessionError, SessionControl, SessionCreateError, SessionDeleteError, SessionId,
    SessionPersistence, SessionStream,
};

use super::worker::{RuntimeCommand, RuntimeWorker};

/// What can go wrong while building an [`AgentRuntime`] and [`AgentService`].
#[derive(Debug, thiserror::Error)]
pub enum AgentRuntimeBuildError {
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
    #[error("persisted session state is missing: {0}")]
    MissingPersistedSessionState(SessionId),
    #[error("failed to reconcile persisted agents: {0}")]
    AgentReconciliation(String),
}

impl From<AgentManagerError> for AgentRuntimeBuildError {
    fn from(error: AgentManagerError) -> Self {
        match error {
            AgentManagerError::MissingPersistenceDir => Self::MissingPersistenceDir,
            AgentManagerError::LongTermInit(source) => Self::LongTermInit(source),
            AgentManagerError::SkillRegistry(source) => Self::SkillRegistry(source),
            AgentManagerError::AgentReconciliation(source) => {
                Self::AgentReconciliation(source.to_string())
            }
        }
    }
}

/// Cloneable control handle for the process-level agent service.
///
/// The framework never starts a thread or chooses an executor. The application
/// must spawn the matching [`AgentService`] future on Embassy (or any other
/// executor) and then use this handle from tasks on that executor.
#[derive(Clone)]
pub struct AgentRuntime {
    commands: Sender<RuntimeCommand>,
    api_manager: SharedApiManager,
}

/// The long-running executor-neutral agent service future.
///
/// Dropping this future stops the runtime. Dropping every [`AgentRuntime`]
/// handle closes its command channel, which lets the service shut down cleanly.
pub struct AgentService<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    worker: RuntimeWorker<Filesystem, Http, Timer>,
}

impl<Filesystem, Http, Timer> Unpin for AgentService<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
}

impl<Filesystem, Http, Timer> Future for AgentService<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().worker).poll(context)
    }
}

impl AgentRuntime {
    /// Build a control handle and its service future without starting an
    /// executor or allocating an OS thread.
    pub fn new<Filesystem, Http, Timer>(
        filesystem: Arc<Filesystem>,
        tool_registry: Arc<ToolRegistry>,
        persistence: SharedPersistence<Filesystem>,
        persistence_dir: String,
        skill_roots: Vec<String>,
    ) -> Result<(Self, AgentService<Filesystem, Http, Timer>), AgentRuntimeBuildError>
    where
        Filesystem: ClawFs + 'static,
        Http: ClawHttp + StreamingHttp + Default + 'static,
        Timer: ClawTimer + Default + 'static,
    {
        let (commands, command_rx) = async_channel::unbounded();
        let api_manager = SharedApiManager::default();
        let worker = RuntimeWorker::new(
            filesystem,
            tool_registry,
            persistence,
            persistence_dir,
            skill_roots,
            Arc::clone(&api_manager),
            command_rx,
        )?;
        Ok((
            Self {
                commands,
                api_manager,
            },
            AgentService { worker },
        ))
    }

    /// Register an LLM API config for a purpose.
    pub fn link_api(
        &self,
        api: ClawApiConfig,
        purpose: ApiPurpose,
        default: bool,
    ) -> Result<(), InitError> {
        self.api_manager
            .borrow_mut()
            .link_api(api, purpose, default)
    }

    /// Open a Session's long-lived event stream.
    pub async fn open_session(
        &self,
        session: SessionId,
    ) -> Result<(SessionControl, SessionStream), OpenSessionError> {
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .send(RuntimeCommand::OpenSession { session, ack })
            .await
            .map_err(|_| OpenSessionError::WorkerStopped)?;
        result
            .recv()
            .await
            .unwrap_or(Err(OpenSessionError::WorkerStopped))
    }

    /// Create a fresh isolated Session.
    pub async fn create_session(
        &self,
        persistence: SessionPersistence,
    ) -> Result<SessionId, SessionCreateError> {
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .send(RuntimeCommand::CreateSession { persistence, ack })
            .await
            .map_err(|_| SessionCreateError::WorkerStopped)?;
        result
            .recv()
            .await
            .unwrap_or(Err(SessionCreateError::WorkerStopped))
    }

    /// Return live Sessions, sorted by id.
    pub async fn list_sessions(&self) -> Vec<SessionId> {
        let (ack, result) = async_channel::bounded(1);
        if self
            .commands
            .send(RuntimeCommand::ListSessions { ack })
            .await
            .is_err()
        {
            return Vec::new();
        }
        result.recv().await.unwrap_or_default()
    }

    /// Delete a live Session and its associated runtime state.
    pub async fn delete_session(&self, session: SessionId) -> Result<(), SessionDeleteError> {
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .send(RuntimeCommand::DeleteSession { session, ack })
            .await
            .map_err(|_| SessionDeleteError::WorkerStopped)?;
        result
            .recv()
            .await
            .unwrap_or(Err(SessionDeleteError::WorkerStopped))
    }

    /// Ask the service to stop after its live actors finish closing.
    pub async fn shutdown(&self) {
        let _ = self.commands.send(RuntimeCommand::Stop).await;
    }
}

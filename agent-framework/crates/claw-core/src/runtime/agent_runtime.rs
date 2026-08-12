//! Executor-neutral process runtime ownership and configuration entry point.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use async_channel::Sender;
use claw_api::{ClawApiConfig, ClawApiFactory, InitError};
use claw_fs::ClawFs;
use claw_memory::LongTermInitError;
use claw_net::{Dns, TcpConnect};
use claw_persistence::{PersistenceError, SharedPersistence};
use claw_skill::SkillError;
use claw_tool::ToolRegistry;
use futures_channel::oneshot;

use crate::agent::{AgentCreateError, AgentManagerError};
use crate::config::{ApiPurpose, SharedApiManager};
use crate::session::{
    OpenSessionError, SessionControl, SessionCreateError, SessionDeleteError, SessionId,
    SessionPersistence, SessionStream,
};

use super::worker::{RuntimeCommand, RuntimeWorker, RuntimeWorkerInit};

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
    AgentReconciliation(#[from] AgentCreateError),
}

impl From<AgentManagerError> for AgentRuntimeBuildError {
    fn from(error: AgentManagerError) -> Self {
        match error {
            AgentManagerError::MissingPersistenceDir => Self::MissingPersistenceDir,
            AgentManagerError::LongTermInit(source) => Self::LongTermInit(source),
            AgentManagerError::SkillRegistry(source) => Self::SkillRegistry(source),
            AgentManagerError::AgentReconciliation(source) => Self::AgentReconciliation(source),
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
pub struct AgentService<Filesystem, Http>
where
    Filesystem: ClawFs + 'static,
    Http: TcpConnect + Dns + 'static,
{
    worker: RuntimeWorker<Filesystem, Http>,
}

#[cfg(test)]
mod tests {
    use core::error::Error as _;

    use super::AgentRuntimeBuildError;
    use crate::agent::{AgentCreateError, AgentManagerError};

    #[test]
    fn reconciliation_failure_preserves_typed_source() {
        let error = AgentRuntimeBuildError::from(AgentManagerError::AgentReconciliation(
            AgentCreateError::UnknownKind("worker".into()),
        ));

        assert!(error.source().is_some());
        assert!(matches!(
            error,
            AgentRuntimeBuildError::AgentReconciliation(AgentCreateError::UnknownKind(kind))
                if kind == "worker"
        ));
    }
}

impl<Filesystem, Http> Unpin for AgentService<Filesystem, Http>
where
    Filesystem: ClawFs + 'static,
    Http: TcpConnect + Dns + 'static,
{
}

impl<Filesystem, Http> Future for AgentService<Filesystem, Http>
where
    Filesystem: ClawFs + 'static,
    Http: TcpConnect + Dns + 'static,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().worker).poll(context)
    }
}

impl AgentRuntime {
    /// Build a control handle and its service future without starting an
    /// executor or allocating an OS thread.
    pub fn new<Filesystem, Http>(
        filesystem: Arc<Filesystem>,
        tool_registry: Arc<ToolRegistry>,
        persistence: SharedPersistence<Filesystem>,
        persistence_dir: String,
        skill_roots: Vec<String>,
        llm_factory: ClawApiFactory<Http>,
    ) -> Result<(Self, AgentService<Filesystem, Http>), AgentRuntimeBuildError>
    where
        Filesystem: ClawFs + 'static,
        Http: TcpConnect + Dns + 'static,
    {
        let (commands, command_rx) = async_channel::unbounded();
        let api_manager = SharedApiManager::default();
        let worker = RuntimeWorker::new(RuntimeWorkerInit {
            filesystem,
            tool_registry,
            persistence,
            persistence_dir,
            skill_roots,
            api_manager: Arc::clone(&api_manager),
            llm_factory,
            commands: command_rx,
        })?;
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
        let (ack, result) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::OpenSession { session, ack })
            .await
            .map_err(|_| OpenSessionError::WorkerStopped)?;
        result.await.unwrap_or(Err(OpenSessionError::WorkerStopped))
    }

    /// Create a fresh isolated Session.
    pub async fn create_session(
        &self,
        persistence: SessionPersistence,
    ) -> Result<SessionId, SessionCreateError> {
        let (ack, result) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::CreateSession { persistence, ack })
            .await
            .map_err(|_| SessionCreateError::WorkerStopped)?;
        result
            .await
            .unwrap_or(Err(SessionCreateError::WorkerStopped))
    }

    /// Return live Sessions, sorted by id.
    pub async fn list_sessions(&self) -> Vec<SessionId> {
        let (ack, result) = oneshot::channel();
        if self
            .commands
            .send(RuntimeCommand::ListSessions { ack })
            .await
            .is_err()
        {
            return Vec::new();
        }
        result.await.unwrap_or_default()
    }

    /// Delete a live Session and its associated runtime state.
    pub async fn delete_session(&self, session: SessionId) -> Result<(), SessionDeleteError> {
        let (ack, result) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::DeleteSession { session, ack })
            .await
            .map_err(|_| SessionDeleteError::WorkerStopped)?;
        result
            .await
            .unwrap_or(Err(SessionDeleteError::WorkerStopped))
    }

    /// Ask the service to stop after its live actors finish closing.
    pub async fn shutdown(&self) {
        let _ = self.commands.send(RuntimeCommand::Stop).await;
    }
}

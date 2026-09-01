//! Executor-neutral process runtime ownership and configuration entry point.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use async_channel::Sender;
use barracuda_agent_memory::LongTermInitError;
use barracuda_agent_persistence::{Persistence, PersistenceError, SharedPersistence};
use barracuda_agent_skill::SkillError;
use barracuda_agent_tool::ToolRegistry;
use barracuda_model_api::{InitError, ModelApiConfig, ModelApiFactory};
use barracuda_vfs::ScopedVfs;
use futures_channel::oneshot;
use http_client::embedded_nal_async::{Dns, TcpConnect};

use barracuda_agent::{AgentCreateError, AgentManagerError, ApiPurpose, SharedApiManager};
use barracuda_agent_session::{
    OpenSessionError, SessionControl, SessionCreateError, SessionDeleteError, SessionId,
    SessionPersistence, SessionStream,
};

use crate::worker::{RuntimeCommand, RuntimeWorker, RuntimeWorkerInit};
use crate::{
    RuntimeError, RuntimeStorageConfig, SharedToolGroupProvider, ToolGroup, ToolLifecycle,
};

/// What can go wrong while building an [`AgentRuntime`](crate::AgentRuntime) and [`RuntimeService`].
#[derive(Debug, thiserror::Error)]
pub enum RuntimeBuildError {
    #[error("persistence directory is required")]
    MissingPersistenceDir,
    #[error("failed to load long-term memory: {0}")]
    LongTermInit(#[from] LongTermInitError),
    #[error("failed to load skill catalog: {0}")]
    SkillRegistry(#[from] SkillError),
    #[error("failed to initialize persistence: {0}")]
    Persistence(#[from] PersistenceError),
    #[error("invalid persisted session id: {0}")]
    InvalidSessionId(#[from] barracuda_runtime_utils::IdParseError),
    #[error("persisted session state is missing: {0}")]
    MissingPersistedSessionState(SessionId),
    #[error("failed to reconcile persisted agents: {0}")]
    AgentReconciliation(#[from] AgentCreateError),
}

impl From<AgentManagerError> for RuntimeBuildError {
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
/// must spawn the matching [`RuntimeService`] future on Embassy (or any other
/// executor) and then use this handle from tasks on that executor.
#[derive(Clone)]
pub(crate) struct RuntimeControl {
    commands: Sender<RuntimeCommand>,
    api_manager: SharedApiManager,
}

/// The long-running executor-neutral agent service future.
///
/// Dropping this future stops the runtime. Dropping every `RuntimeControl`
/// handle closes its command channel, which lets the service shut down cleanly.
pub struct RuntimeService {
    future: Pin<Box<dyn Future<Output = ()>>>,
}

impl Unpin for RuntimeService {}

impl Future for RuntimeService {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().future.as_mut().poll(context)
    }
}

impl RuntimeControl {
    /// Build a control handle and its service future without starting an
    /// executor or allocating an OS thread.
    pub(crate) fn new<Tcp, Resolver>(
        filesystem: ScopedVfs,
        storage: RuntimeStorageConfig,
        llm_factory: ModelApiFactory<Tcp, Resolver>,
        tool_groups: Vec<ToolGroup>,
        tool_group_providers: Vec<SharedToolGroupProvider>,
        tool_lifecycle: Arc<ToolLifecycle>,
    ) -> (Self, RuntimeService)
    where
        Tcp: TcpConnect + 'static,
        Resolver: Dns + 'static,
    {
        let (commands, command_rx) = async_channel::unbounded();
        let api_manager = SharedApiManager::default();
        let worker_api_manager = Arc::clone(&api_manager);
        let future = Box::pin(async move {
            let initialized: Result<RuntimeWorker<Tcp, Resolver>, RuntimeError> = async {
                let persistence: SharedPersistence = Arc::new(
                    Persistence::new(filesystem.clone(), storage.persistence_root.clone()).await?,
                );
                let tools = Arc::new(ToolRegistry::new(Arc::clone(&persistence)).await?);
                for group in tool_groups {
                    tools.register_group(group)?;
                }
                for provider in tool_group_providers {
                    for group in provider.provide()? {
                        tools.register_group(group)?;
                    }
                }
                tool_lifecycle.install(Arc::clone(&tools))?;
                RuntimeWorker::new(RuntimeWorkerInit {
                    filesystem,
                    tool_registry: tools,
                    persistence,
                    persistence_dir: storage.persistence_root,
                    skill_roots: storage.skill_roots,
                    api_manager: worker_api_manager,
                    llm_factory,
                    commands: command_rx,
                })
                .await
                .map_err(RuntimeError::Build)
            }
            .await;
            match initialized {
                Ok(worker) => worker.await,
                Err(error) => {
                    log::error!("agent runtime initialization failed: {error}");
                    tracing::error!(name: "runtime_initialization_failed", error = %error);
                }
            }
        });
        (
            Self {
                commands,
                api_manager,
            },
            RuntimeService { future },
        )
    }

    /// Register an LLM API config for a purpose.
    pub(crate) fn set_api(
        &self,
        api: ModelApiConfig,
        purpose: ApiPurpose,
        default: bool,
    ) -> Result<(), InitError> {
        self.api_manager.borrow_mut().set_api(api, purpose, default)
    }

    /// Open a Session's long-lived event stream.
    pub(crate) async fn open_session(
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
    pub(crate) async fn create_session(
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
    pub(crate) async fn list_sessions(&self) -> Vec<SessionId> {
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
    pub(crate) async fn delete_session(
        &self,
        session: SessionId,
    ) -> Result<(), SessionDeleteError> {
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
    pub(crate) async fn shutdown(&self) {
        let _ = self.commands.send(RuntimeCommand::Stop).await;
    }
}

#[cfg(test)]
mod tests {
    use core::error::Error as _;

    use super::RuntimeBuildError;
    use barracuda_agent::{AgentCreateError, AgentManagerError};

    #[test]
    fn reconciliation_failure_preserves_typed_source() {
        let error = RuntimeBuildError::from(AgentManagerError::AgentReconciliation(
            AgentCreateError::UnknownKind("worker".into()),
        ));

        assert!(error.source().is_some());
        assert!(matches!(
            error,
            RuntimeBuildError::AgentReconciliation(AgentCreateError::UnknownKind(kind))
                if kind == "worker"
        ));
    }
}

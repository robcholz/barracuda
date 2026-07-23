//! Process runtime ownership and configuration entry point.

use std::io;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use async_channel::Sender;
use claw_api::{ClawApiConfig, InitError};
use claw_interface::http::StreamingHttp;
use claw_interface::{
    ClawExecutor, ClawFs, ClawHttp, ClawThread, ClawTimer, CoreAffinity, Priority, WorkerHandle,
};
use claw_memory::LongTermInitError;
use claw_persistence::{PersistenceError, SharedPersistence};
use claw_skill::SkillError;
use claw_tool::ToolRegistry;

use crate::agent::AgentManagerError;
use crate::config::{ApiUsage, SharedApiManager};
use crate::session::{
    OpenSessionError, SessionControlError, SessionCreateError, SessionId, SessionPersistence,
    SessionStream,
};
use crate::SYSTEM_TRACE_SCOPE;

use super::worker::{run_runtime_worker, RuntimeCommand};

pub(super) const RUNTIME_TRACE_TASK: &str = "agent-runtime";
const RUNTIME_WORKER_STACK_SIZE: usize = 64 * 1024;

/// What can go wrong while building an [`AgentRuntime`].
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
    #[error("failed to spawn the agent runtime worker: {0}")]
    WorkerSpawn(#[from] io::Error),
    #[error("agent runtime worker exited before signalling readiness")]
    WorkerExitedBeforeReady,
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

/// The process-level execution runtime.
///
/// Cloning is intentionally not provided: the handle owns the worker's lifetime
/// and joins it on drop. Wrap it in an `Arc` to share.
pub struct AgentRuntime {
    commands: Sender<RuntimeCommand>,
    worker: Mutex<Option<WorkerHandle>>,
    /// Shared with the worker: turns read the per-usage config from it at
    /// each iteration; this handle side updates it via [`link_api`](Self::link_api).
    api_manager: SharedApiManager,
}

impl AgentRuntime {
    /// Start the runtime worker and restore persistent runtime metadata.
    ///
    /// # Errors
    ///
    /// Returns [`AgentRuntimeBuildError`] when persistent state cannot be
    /// restored or the worker cannot be started.
    pub fn new<Filesystem, Http, Timer, Thread, Executor>(
        tools: Arc<ToolRegistry>,
        persistence: SharedPersistence<Filesystem>,
        persistence_dir: String,
        skill_roots: Vec<String>,
    ) -> Result<Self, AgentRuntimeBuildError>
    where
        Filesystem: ClawFs + 'static,
        Http: ClawHttp + StreamingHttp + Default + 'static,
        Timer: ClawTimer + Default + 'static,
        Thread: ClawThread,
        Executor: ClawExecutor + 'static,
    {
        let (commands, command_rx) = async_channel::unbounded();
        let (init_result_tx, ready_rx) = mpsc::channel();

        let api_manager = SharedApiManager::default();
        let worker_api_manager = Arc::clone(&api_manager);

        let worker = Thread::spawn_worker(
            "claw_agent_runtime",
            RUNTIME_WORKER_STACK_SIZE,
            Priority::Normal,
            CoreAffinity::Any,
            move || {
                run_runtime_worker::<Filesystem, Http, Timer, Executor>(
                    tools,
                    persistence,
                    persistence_dir,
                    skill_roots,
                    command_rx,
                    init_result_tx,
                    worker_api_manager,
                );
            },
        )?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                worker: Mutex::new(Some(worker)),
                api_manager,
            }),
            Ok(Err(error)) => {
                worker.join();
                Err(error)
            }
            Err(_) => {
                worker.join();
                Err(AgentRuntimeBuildError::WorkerExitedBeforeReady)
            }
        }
    }

    /// Register an LLM API config for a usage.
    ///
    /// Takes `&self`: the manager is behind an `RwLock`, and updates are picked up
    /// at the start of the next Agent iteration, so this never interrupts an
    /// in-flight LLM/tool operation.
    ///
    /// # Errors
    ///
    /// Returns [`InitError`] without changing bindings when `api` is invalid.
    pub fn link_api(
        &self,
        api: ClawApiConfig,
        usage: ApiUsage,
        default: bool,
    ) -> Result<(), InitError> {
        self.api_manager
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .link_api(api, usage, default)
    }

    /// Open a Session's long-lived event stream.
    ///
    /// # Errors
    ///
    /// Returns [`OpenSessionError`] when the Session cannot be opened.
    pub fn open_session(&self, session: SessionId) -> Result<SessionStream, OpenSessionError> {
        let span = tracing::info_span!(
            "session",
            run.system = SYSTEM_TRACE_SCOPE,
            run.session = %session,
        );
        let _enter = span.enter();
        let (events, receiver) = async_channel::unbounded();
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .try_send(RuntimeCommand::OpenSession {
                session,
                events,
                ack,
            })
            .map_err(|_| {
                tracing::error!(name: "open_rejected", reason = "runtime_stopped");
                OpenSessionError::WorkerStopped
            })?;
        match result.recv_blocking() {
            Ok(Ok(endpoint)) => {
                tracing::info!(name: "opened", "");
                Ok(SessionStream::new(endpoint, receiver))
            }
            Ok(Err(error)) => {
                match &error {
                    OpenSessionError::SessionNotFound(_) => {
                        tracing::warn!(name: "open_rejected", reason = "session_not_found");
                    }
                    OpenSessionError::AlreadyOpen(_) => {
                        tracing::warn!(name: "open_rejected", reason = "already_open");
                    }
                    OpenSessionError::WorkerStopped => {
                        tracing::error!(name: "open_rejected", reason = "runtime_stopped");
                    }
                }
                Err(error)
            }
            Err(_) => {
                tracing::error!(name: "open_rejected", reason = "runtime_stopped");
                Err(OpenSessionError::WorkerStopped)
            }
        }
    }

    /// Create a fresh isolated Session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionCreateError`] when the Session cannot be created.
    pub fn create_session(
        &self,
        persistence: SessionPersistence,
    ) -> Result<SessionId, SessionCreateError> {
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .send_blocking(RuntimeCommand::CreateSession { persistence, ack })
            .map_err(|_| SessionCreateError::WorkerStopped)?;
        let session = result
            .recv_blocking()
            .unwrap_or(Err(SessionCreateError::WorkerStopped))?;
        let span = tracing::info_span!(
            "session.create",
            run.system = SYSTEM_TRACE_SCOPE,
            run.session = %session,
        );
        let _enter = span.enter();
        tracing::info!(name: "created", persistence = ?persistence);
        Ok(session)
    }

    /// Return live Sessions, sorted by id.
    pub fn list_sessions(&self) -> Vec<SessionId> {
        let (ack, result) = async_channel::bounded(1);
        if self
            .commands
            .send_blocking(RuntimeCommand::ListSessions { ack })
            .is_err()
        {
            return Vec::new();
        }
        result.recv_blocking().unwrap_or_default()
    }

    /// Delete a live Session and its associated runtime state.
    ///
    /// # Errors
    ///
    /// Returns [`SessionControlError`] when the Session cannot be deleted.
    pub fn delete_session(&self, session: SessionId) -> Result<(), SessionControlError> {
        let span = tracing::info_span!(
            "session.delete",
            run.system = SYSTEM_TRACE_SCOPE,
            run.session = %session,
        );
        let _enter = span.enter();
        let (ack, result) = async_channel::bounded(1);
        self.commands
            .try_send(RuntimeCommand::DeleteSession { session, ack })
            .map_err(|_| {
                tracing::error!(name: "delete_rejected", reason = "runtime_stopped");
                SessionControlError::WorkerStopped
            })?;
        match result.recv_blocking() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                match &error {
                    SessionControlError::SessionClosed(_) => {
                        tracing::warn!(name: "delete_rejected", reason = "session_closed");
                    }
                    SessionControlError::WorkerStopped => {
                        tracing::error!(name: "delete_rejected", reason = "runtime_stopped");
                    }
                    SessionControlError::Persistence => {
                        tracing::error!(name: "delete_rejected", reason = "persistence");
                    }
                    SessionControlError::NotAwaitingInput(_)
                    | SessionControlError::InputRequestMismatch { .. } => {
                        tracing::error!(name: "delete_rejected", reason = "unexpected_response");
                    }
                }
                Err(error)
            }
            Err(_) => {
                tracing::error!(name: "delete_rejected", reason = "runtime_stopped");
                Err(SessionControlError::WorkerStopped)
            }
        }
    }
}

impl Drop for AgentRuntime {
    fn drop(&mut self) {
        let span = tracing::info_span!(
            "agent.runtime.shutdown",
            trace.task = RUNTIME_TRACE_TASK,
            run.system = SYSTEM_TRACE_SCOPE,
        );
        let _enter = span.enter();
        let _ = self.commands.try_send(RuntimeCommand::Stop);
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            worker.join();
        }
    }
}

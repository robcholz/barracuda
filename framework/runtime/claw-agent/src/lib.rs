//! `claw_agent` assembles tools, persistence, and the core agent runtime.
//!
//! `AgentSystem` owns sessions and exposes session connections. Transport
//! routing, channel inbound/outbound conversion, and reply destinations live in
//! adapter crates above this layer.

use std::marker::PhantomData;
use std::sync::Arc;

use claw_api::{ClawApiConfig, InitError};
pub use claw_core::stream;
pub use claw_core::{
    AgentId, ApiUsage, InputRequestId, InputRequestKind, IterationId, Message, OpenSessionError,
    PermissionLevel, ReasoningEffort, SessionControl, SessionControlError, SessionCreateError,
    SessionEvent, SessionId, SessionPersistence, SessionStream, ToolCall, TurnId, TurnOrigin,
};
use claw_core::{AgentRuntime, AgentRuntimeBuildError};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawExecutor, ClawFs, ClawHttp, ClawThread, ClawTimer, FsError};
#[cfg(feature = "host-backends")]
use claw_interface::{DiskFs, RealHttp, TokioTimer};
use claw_persistence::{DurableState, Persistence, PersistenceError, SharedPersistence};
use claw_tool::{ToolRegistry, ToolRegistryError, ToolRegistryState};

const TOOL_REGISTRY_ENTRY: &str = "tool_registry";

#[cfg(feature = "host-backends")]
pub type HostAgentSystem = AgentSystem<DiskFs, RealHttp, TokioTimer>;

pub type AgentResult<T> = Result<T, AgentError>;

/// Explicit storage root for an [`AgentSystem`], plus the skill roots the agent
/// factory scans to populate every agent's skill catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPersistenceConfig {
    pub persistence_root: String,
    /// Skill roots in priority order (e.g. DATA before SYSTEM). Empty means no
    /// filesystem skills are loaded.
    pub skill_roots: Vec<String>,
}

/// What can go wrong while building or driving an [`AgentSystem`].
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// An LLM API config could not be linked because a required field is empty.
    #[error(transparent)]
    LlmConfig(#[from] InitError),
    /// Building the core agent runtime failed.
    #[error(transparent)]
    Runtime(#[from] AgentRuntimeBuildError),
    /// The tool registry failed.
    #[error(transparent)]
    Tool(#[from] ToolRegistryError),
    /// Opening a session event stream failed.
    #[error(transparent)]
    OpenSession(#[from] OpenSessionError),
    /// Creating a session through the session manager failed.
    #[error(transparent)]
    SessionCreate(#[from] SessionCreateError),
    /// The scratch storage root could not be cleared before startup.
    #[error("failed to clear agent storage at {path}: {source}")]
    StorageClear {
        path: String,
        #[source]
        source: FsError,
    },
    /// Runtime state could not be loaded or written.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

/// A ready-to-drive agent runtime.
///
/// The `Filesystem`/`Http`/`Timer` backends select which concrete filesystem,
/// HTTP, and timer the core runtime worker uses; they are only needed at
/// construction, so they are held as a marker (the built [`AgentRuntime`] handle
/// is backend-erased and `Send + Sync`).
type BackendMarker<Filesystem, Http, Timer> = PhantomData<fn() -> (Filesystem, Http, Timer)>;

pub struct AgentSystem<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    tools: Arc<ToolRegistry>,
    runtime: AgentRuntime,
    _marker: BackendMarker<Filesystem, Http, Timer>,
}

impl<Filesystem, Http, Timer> AgentSystem<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    /// Build a fully injectable agent system, spawning the core runtime
    /// worker via the [`ClawThread`] policy `Thread` (`StdThread` on host,
    /// `EspIdfThread` on device) and driving its `!Send` engine with the injected
    /// [`ClawExecutor`] `Executor` (`TokioExecutor` on host,
    /// `EspIdfExecutor` on device).
    /// Both are zero-sized policies selected purely by type parameter, like the
    /// `Filesystem`/`Http`/`Timer` backends.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError`] when storage cleanup or runtime construction fails.
    pub fn new<Thread, Executor>(persistence: AgentPersistenceConfig) -> AgentResult<Self>
    where
        Thread: ClawThread,
        Executor: ClawExecutor + 'static,
    {
        let shared_persistence: SharedPersistence<Filesystem> =
            Arc::new(Persistence::new(persistence.persistence_root.clone())?);
        let tool_registry =
            shared_persistence.singleton::<ToolRegistryState>(TOOL_REGISTRY_ENTRY)?;
        let tool_registry_dto = tool_registry.load()?.unwrap_or_default();
        let tool_registry_state = DurableState::new(tool_registry_dto);
        tool_registry.register(&tool_registry_state)?;
        let tools = Arc::new(ToolRegistry::from_state(tool_registry_state));
        let runtime = AgentRuntime::new::<Filesystem, Http, Timer, Thread, Executor>(
            Arc::clone(&tools),
            shared_persistence,
            persistence.persistence_root,
            persistence.skill_roots,
        )?;

        Ok(Self {
            tools,
            runtime,
            _marker: PhantomData,
        })
    }

    /// Tool registry used by this system.
    pub fn tool_registry(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Start every registered tool.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError`] when the tool registry fails to start.
    pub fn start_all(&self) -> AgentResult<()> {
        self.tools.start_all()?;
        Ok(())
    }

    /// Stop every registered tool.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError`] when the tool registry fails to stop.
    pub fn stop_all(&self) -> AgentResult<()> {
        self.tools.stop_all()?;
        Ok(())
    }

    /// Open a live session's long-lived event stream and control surface.
    ///
    /// # Errors
    ///
    /// Returns [`OpenSessionError`] when the session is missing, already open, or
    /// the runtime is stopped.
    pub fn open_session(&self, session: SessionId) -> AgentResult<SessionStream> {
        Ok(self.runtime.open_session(session)?)
    }

    /// Register an LLM API config for a usage (root/subagent/memory/compaction).
    ///
    /// De-duplicated by model; when `default` is set it becomes the fallback for
    /// usages without an explicit binding. Updates take effect at the start of the
    /// next Agent iteration, so this never disturbs an in-flight LLM/tool operation.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::LlmConfig`] without changing bindings when `api` is
    /// invalid.
    pub fn link_api(&self, api: ClawApiConfig, usage: ApiUsage, default: bool) -> AgentResult<()> {
        self.runtime.link_api(api, usage, default)?;
        Ok(())
    }

    /// Create a fresh isolated conversation session with explicit persistence.
    /// Ephemeral sessions keep their transcript only for this process.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::SessionCreate`] if persistent session state cannot
    /// be initialized or the runtime has stopped.
    pub fn new_session(&self, persistence: SessionPersistence) -> AgentResult<SessionId> {
        self.runtime
            .create_session(persistence)
            .map_err(AgentError::from)
    }

    /// Return the live conversation sessions.
    pub fn list_sessions(&self) -> Vec<SessionId> {
        self.runtime.list_sessions()
    }

    /// Delete a live conversation session.
    ///
    /// If the session is currently open, its event stream receives
    /// [`SessionEvent::Closed`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionControlError`] when the session is already gone or the
    /// runtime is stopped.
    pub fn delete_session(&self, session: SessionId) -> Result<(), SessionControlError> {
        self.runtime.delete_session(session)
    }
}

//! `claw_agent` assembles tools, persistence, and the core agent runtime.
//!
//! `AgentSystem` owns sessions and exposes session connections. Transport
//! routing, channel inbound/outbound conversion, and reply destinations live in
//! adapter crates above this layer.

#![no_std]
// Public subsystem handles share ownership inside one AgentService task.
#![allow(clippy::arc_with_non_send_sync)]

extern crate alloc;

use alloc::{string::String, sync::Arc, vec::Vec};
use core::marker::PhantomData;

use claw_api::InitError;
#[cfg(feature = "cache_profile")]
pub use claw_api::ProviderUsage;
pub use claw_api::{BackendKind, ClawApiConfig, ClawApiFactory};
pub use claw_core::stream;
pub use claw_core::AgentService;
pub use claw_core::{
    AgentApprovalError, AgentCreateError, AgentId, ApiPurpose, ApprovalResolverError,
    BaseAgentError, ContextProviderError, InputRequestId, InputRequestKind, IterationEvent,
    IterationId, IterationLoopError, Message, OpenSessionError, PermissionLevel, ReasoningEffort,
    SessionCloseReason, SessionControl, SessionControlError, SessionCreateError,
    SessionDeleteError, SessionError, SessionEvent, SessionEventError, SessionId,
    SessionInputError, SessionPersistence, SessionStream, SessionTurnError, ToolCall, ToolCallId,
    ToolOutput, TurnEvent, TurnEventError, TurnId, TurnOrigin,
};
use claw_core::{AgentRuntime, AgentRuntimeBuildError};
use claw_fs::{ClawFs, FsError};
use claw_net::{Dns, TcpConnect};
use claw_persistence::{Persistence, PersistenceError, SharedPersistence};
use claw_tool::{ToolRegistry, ToolRegistryError};

/// Types needed to define tools accepted by [`AgentSystem::with_tool_groups`].
pub mod tools {
    pub use claw_tool::{
        tool_metadata, Action, EmptyArgs, Resource, RiskClass, Tool, ToolConfig, ToolError,
        ToolFuture, ToolGroup, ToolHandler, ToolInvocation, ToolInvokeError, ToolOutput,
        ToolResult, ToolSpec,
    };
}

pub use tools::ToolGroup;

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
/// The `Filesystem`/`Http` parameters record which concrete backends the
/// core runtime worker owns. The backend-erased [`AgentRuntime`] handle retains
/// the actual filesystem instance; this marker only preserves the public
/// `AgentSystem` type relationship.
type BackendMarker<Filesystem, Http> = PhantomData<fn() -> (Filesystem, Http)>;

pub struct AgentSystem<Filesystem, Http>
where
    Filesystem: ClawFs + 'static,
    Http: TcpConnect + Dns + 'static,
{
    tools: Arc<ToolRegistry>,
    runtime: AgentRuntime,
    _marker: BackendMarker<Filesystem, Http>,
}

impl<Filesystem, Http> AgentSystem<Filesystem, Http>
where
    Filesystem: ClawFs + 'static,
    Http: TcpConnect + Dns + 'static,
{
    /// Build an agent system with an empty tool registry.
    ///
    /// The returned service future must be spawned by the application. The
    /// framework does not create a thread or choose an executor.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError`] when storage cleanup or runtime construction fails.
    pub fn new(
        filesystem: Filesystem,
        persistence: AgentPersistenceConfig,
        llm_factory: ClawApiFactory<Http>,
    ) -> AgentResult<(Self, AgentService<Filesystem, Http>)> {
        Self::with_tool_groups(
            filesystem,
            persistence,
            llm_factory,
            core::iter::empty::<ToolGroup>(),
        )
    }

    /// Build a fully injectable agent system with its initial tool groups.
    ///
    /// The tool registry and runtime are bound to the same persistence owner.
    /// Registering groups during construction keeps that ownership relationship
    /// internal while still allowing host and device adapters to supply tools.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError`] when persistence, tool registration, or runtime
    /// construction fails.
    pub fn with_tool_groups(
        filesystem: Filesystem,
        persistence: AgentPersistenceConfig,
        llm_factory: ClawApiFactory<Http>,
        tool_groups: impl IntoIterator<Item = ToolGroup>,
    ) -> AgentResult<(Self, AgentService<Filesystem, Http>)> {
        let filesystem = Arc::new(filesystem);
        let shared_persistence: SharedPersistence<Filesystem> = Arc::new(Persistence::new(
            Arc::clone(&filesystem),
            persistence.persistence_root.clone(),
        )?);
        let tools = Arc::new(ToolRegistry::new(Arc::clone(&shared_persistence))?);
        for group in tool_groups {
            tools.register_group(group)?;
        }
        let (runtime, service) = AgentRuntime::new::<Filesystem, Http>(
            filesystem,
            Arc::clone(&tools),
            shared_persistence,
            persistence.persistence_root,
            persistence.skill_roots,
            llm_factory,
        )?;

        Ok((
            Self {
                tools,
                runtime,
                _marker: PhantomData,
            },
            service,
        ))
    }

    /// Enable a registered tool.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Tool`] when the tool is not registered.
    pub fn enable_tool(&self, name: &str) -> AgentResult<()> {
        self.tools.enable(name)?;
        Ok(())
    }

    /// Disable a registered tool.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Tool`] when the tool is not registered.
    pub fn disable_tool(&self, name: &str) -> AgentResult<()> {
        self.tools.disable(name)?;
        Ok(())
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
    pub async fn open_session(
        &self,
        session: SessionId,
    ) -> AgentResult<(SessionControl, SessionStream)> {
        Ok(self.runtime.open_session(session).await?)
    }

    /// Register an LLM API config for a purpose (root/subagent/memory/compaction).
    ///
    /// De-duplicated by model; when `default` is set it becomes the fallback for
    /// purposes without an explicit binding. Updates take effect at the start of
    /// the next Agent iteration, so this never disturbs an in-flight LLM/tool
    /// operation.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::LlmConfig`] without changing bindings when `api` is
    /// invalid.
    pub fn link_api(
        &self,
        api: ClawApiConfig,
        purpose: ApiPurpose,
        default: bool,
    ) -> AgentResult<()> {
        self.runtime.link_api(api, purpose, default)?;
        Ok(())
    }

    /// Create a fresh isolated conversation session with explicit persistence.
    /// Ephemeral sessions keep their transcript only for this process.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::SessionCreate`] if persistent session state cannot
    /// be initialized or the runtime has stopped.
    pub async fn new_session(&self, persistence: SessionPersistence) -> AgentResult<SessionId> {
        self.runtime
            .create_session(persistence)
            .await
            .map_err(AgentError::from)
    }

    /// Return the live conversation sessions.
    pub async fn list_sessions(&self) -> Vec<SessionId> {
        self.runtime.list_sessions().await
    }

    /// Delete a live conversation session.
    ///
    /// If the session is currently open, its event stream receives
    /// [`SessionEvent::Closed`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionDeleteError`] if any part of permanent deletion fails.
    pub async fn delete_session(&self, session: SessionId) -> Result<(), SessionDeleteError> {
        self.runtime.delete_session(session).await
    }

    /// Ask the matching service future to shut down.
    pub async fn shutdown(&self) {
        self.runtime.shutdown().await;
    }
}

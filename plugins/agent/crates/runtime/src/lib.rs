//! `barracuda_agent_runtime` assembles agents, sessions, tools, and persistence.
//!
//! `AgentRuntime` owns sessions and exposes session connections. Transport
//! routing, channel inbound/outbound conversion, and reply destinations live in
//! adapter crates above this layer.

#![no_std]
// Public subsystem handles share ownership inside one RuntimeService task.
#![allow(clippy::arc_with_non_send_sync)]

extern crate alloc;

mod service;
mod worker;

use alloc::{string::String, sync::Arc, vec::Vec};
use core::{
    cell::{Cell, RefCell},
    marker::PhantomData,
};

pub use barracuda_agent::stream;
pub use barracuda_agent::{
    AgentApprovalError, AgentCreateError, AgentId, ApiPurpose, IterationId, IterationLoopError,
    Message, PermissionLevel, ReasoningEffort, ToolCall, ToolCallId, ToolOutput,
};
use barracuda_agent_persistence::PersistenceError;
pub use barracuda_agent_session::{
    ApprovalResolverError, ContextProviderError, InputRequestId, InputRequestKind, IterationEvent,
    OpenSessionError, SessionCloseReason, SessionControl, SessionControlError, SessionCreateError,
    SessionDeleteError, SessionError, SessionEvent, SessionEventError, SessionId,
    SessionInputError, SessionPersistence, SessionStream, SessionTurnError, TurnEvent,
    TurnEventError, TurnId, TurnOrigin,
};
use barracuda_agent_tool::{ToolRegistry, ToolRegistryError};
use barracuda_model_api::InitError;
#[cfg(feature = "cache_profile")]
pub use barracuda_model_api::ProviderUsage;
pub use barracuda_model_api::{BackendKind, ModelApiConfig, ModelApiFactory};
use barracuda_vfs::{FsError, ScopedVfs};
use embedded_nal_async::{Dns, TcpConnect};
use service::RuntimeControl;
pub use service::{RuntimeBuildError, RuntimeService};

/// Types needed to define tools accepted by [`AgentRuntime::with_tool_groups`].
pub mod tools {
    pub use barracuda_agent_tool::{
        tool_metadata, Action, EmptyArgs, Resource, RiskClass, Tool, ToolConfig, ToolError,
        ToolFuture, ToolGroup, ToolHandler, ToolInvocation, ToolInvokeError, ToolOutput,
        ToolResult, ToolSpec,
    };
}

pub use tools::ToolGroup;

pub type RuntimeResult<T> = Result<T, RuntimeError>;

/// Explicit storage root for an [`AgentRuntime`], plus the skill roots the agent
/// factory scans to populate every agent's skill catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeStorageConfig {
    pub persistence_root: String,
    /// Skill roots in priority order (e.g. DATA before SYSTEM). Empty means no
    /// filesystem skills are loaded.
    pub skill_roots: Vec<String>,
}

/// What can go wrong while building or driving an [`AgentRuntime`].
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// An LLM API config could not be set because a required field is empty.
    #[error(transparent)]
    LlmConfig(#[from] InitError),
    /// Building the agent runtime failed.
    #[error(transparent)]
    Build(#[from] RuntimeBuildError),
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
    /// The asynchronous service has not finished loading durable state.
    #[error("agent runtime is not ready")]
    NotReady,
}

/// A ready-to-drive agent runtime.
///
/// The `Filesystem`/`Tcp`/`Resolver` parameters record which concrete backends the
/// service worker owns. The backend-erased [`AgentRuntime`] handle retains
/// the actual filesystem instance; this marker only preserves the public
/// `AgentRuntime` type relationship.
type BackendMarker<Tcp, Resolver> = PhantomData<fn() -> (Tcp, Resolver)>;

#[derive(Default)]
pub(crate) struct ToolLifecycle {
    loaded: RefCell<Option<Arc<ToolRegistry>>>,
    started: Cell<bool>,
}

impl ToolLifecycle {
    fn registry(&self) -> RuntimeResult<Arc<ToolRegistry>> {
        self.loaded
            .borrow()
            .as_ref()
            .cloned()
            .ok_or(RuntimeError::NotReady)
    }

    pub(crate) fn install(&self, tools: Arc<ToolRegistry>) -> RuntimeResult<()> {
        if self.started.get() {
            tools.start_all()?;
        }
        *self.loaded.borrow_mut() = Some(tools);
        Ok(())
    }
}

pub struct AgentRuntime<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    tool_lifecycle: Arc<ToolLifecycle>,
    control: RuntimeControl,
    _marker: BackendMarker<Tcp, Resolver>,
}

impl<Tcp, Resolver> AgentRuntime<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    /// Build an agent runtime with an empty tool registry.
    ///
    /// The returned service future must be spawned by the application. The
    /// framework does not create a thread or choose an executor.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when storage cleanup or runtime construction fails.
    pub fn new(
        filesystem: ScopedVfs,
        persistence: RuntimeStorageConfig,
        llm_factory: ModelApiFactory<Tcp, Resolver>,
    ) -> RuntimeResult<(Self, RuntimeService<Tcp, Resolver>)> {
        Self::with_tool_groups(
            filesystem,
            persistence,
            llm_factory,
            core::iter::empty::<ToolGroup>(),
        )
    }

    /// Build a fully injectable agent runtime with its initial tool groups.
    ///
    /// The tool registry and runtime are bound to the same persistence owner.
    /// Registering groups during construction keeps that ownership relationship
    /// internal while still allowing host and device adapters to supply tools.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when persistence, tool registration, or runtime
    /// construction fails.
    pub fn with_tool_groups(
        filesystem: ScopedVfs,
        persistence: RuntimeStorageConfig,
        llm_factory: ModelApiFactory<Tcp, Resolver>,
        tool_groups: impl IntoIterator<Item = ToolGroup>,
    ) -> RuntimeResult<(Self, RuntimeService<Tcp, Resolver>)> {
        let tool_lifecycle = Arc::new(ToolLifecycle::default());
        let (control, service) = RuntimeControl::new::<Tcp, Resolver>(
            filesystem,
            persistence,
            llm_factory,
            tool_groups.into_iter().collect(),
            Arc::clone(&tool_lifecycle),
        );

        Ok((
            Self {
                control,
                tool_lifecycle,
                _marker: PhantomData,
            },
            service,
        ))
    }

    /// Enable a registered tool.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::Tool`] when the tool is not registered.
    pub fn enable_tool(&self, name: &str) -> RuntimeResult<()> {
        self.tool_lifecycle.registry()?.enable(name)?;
        Ok(())
    }

    /// Disable a registered tool.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::Tool`] when the tool is not registered.
    pub fn disable_tool(&self, name: &str) -> RuntimeResult<()> {
        self.tool_lifecycle.registry()?.disable(name)?;
        Ok(())
    }

    /// Start every registered tool.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the tool registry fails to start.
    pub fn start_all(&self) -> RuntimeResult<()> {
        self.tool_lifecycle.started.set(true);
        if let Some(tools) = self.tool_lifecycle.loaded.borrow().as_ref() {
            tools.start_all()?;
        }
        Ok(())
    }

    /// Stop every registered tool.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the tool registry fails to stop.
    pub fn stop_all(&self) -> RuntimeResult<()> {
        self.tool_lifecycle.started.set(false);
        if let Some(tools) = self.tool_lifecycle.loaded.borrow().as_ref() {
            tools.stop_all()?;
        }
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
    ) -> RuntimeResult<(SessionControl, SessionStream)> {
        Ok(self.control.open_session(session).await?)
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
    /// Returns [`InitError`] without changing bindings when `api` is invalid.
    pub fn set_api(
        &self,
        api: ModelApiConfig,
        purpose: ApiPurpose,
        default: bool,
    ) -> Result<(), InitError> {
        self.control.set_api(api, purpose, default)
    }

    /// Create a fresh isolated conversation session with explicit persistence.
    /// Ephemeral sessions keep their transcript only for this process.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::SessionCreate`] if persistent session state cannot
    /// be initialized or the runtime has stopped.
    pub async fn new_session(&self, persistence: SessionPersistence) -> RuntimeResult<SessionId> {
        self.control
            .create_session(persistence)
            .await
            .map_err(RuntimeError::from)
    }

    /// Return the live conversation sessions.
    pub async fn list_sessions(&self) -> Vec<SessionId> {
        self.control.list_sessions().await
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
        self.control.delete_session(session).await
    }

    /// Ask the matching service future to shut down.
    pub async fn shutdown(&self) {
        self.control.shutdown().await;
    }
}

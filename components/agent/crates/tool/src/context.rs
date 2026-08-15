use alloc::string::{String, ToString};
use alloc::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::ToolGroupId;

/// Raw Agent-owned JSON storage used by the tool runtime.
///
/// Implementations own the Agent lifecycle. The tool layer supplies only the
/// already-resolved tool-group namespace, so handlers cannot select another
/// Agent or another group's object.
pub trait AgentStorageBackend: 'static {
    fn load(&self, group: &str) -> Result<Option<Value>, AgentStorageError>;

    fn store(&self, group: &str, object: Value) -> Result<(), AgentStorageError>;

    fn clear(&self, group: &str) -> Result<(), AgentStorageError>;
}

/// Agent storage source supplied to a [`ToolRunner`](crate::ToolRunner).
#[derive(Clone, Default)]
pub struct AgentStorageScope {
    backend: Option<Arc<dyn AgentStorageBackend>>,
}

impl AgentStorageScope {
    pub fn new(backend: Arc<dyn AgentStorageBackend>) -> Self {
        Self {
            backend: Some(backend),
        }
    }

    pub fn unavailable() -> Self {
        Self::default()
    }

    pub(crate) fn bind(&self, group: ToolGroupId) -> AgentStorage {
        AgentStorage {
            backend: self.backend.clone(),
            group,
        }
    }
}

/// Typed JSON object storage bound to one Agent and one tool group.
#[derive(Clone)]
pub struct AgentStorage {
    backend: Option<Arc<dyn AgentStorageBackend>>,
    group: ToolGroupId,
}

impl AgentStorage {
    pub fn load<T>(&self) -> Result<Option<T>, AgentStorageError>
    where
        T: DeserializeOwned,
    {
        self.backend()?
            .load(&self.group)?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| AgentStorageError::Decode(error.to_string()))
    }

    pub fn store<T>(&self, value: &T) -> Result<(), AgentStorageError>
    where
        T: Serialize,
    {
        let object = serde_json::to_value(value)
            .map_err(|error| AgentStorageError::Encode(error.to_string()))?;
        self.backend()?.store(&self.group, object)
    }

    pub fn clear(&self) -> Result<(), AgentStorageError> {
        self.backend()?.clear(&self.group)
    }

    fn backend(&self) -> Result<&dyn AgentStorageBackend, AgentStorageError> {
        self.backend
            .as_deref()
            .ok_or(AgentStorageError::Unavailable)
    }
}

/// Framework-owned context supplied to one tool invocation.
pub struct ToolContext {
    pub agent_storage: AgentStorage,
}

impl ToolContext {
    pub(crate) fn new(agent_storage: AgentStorage) -> Self {
        Self { agent_storage }
    }

    /// Context for internal protocols and focused handler tests that do not run
    /// inside an Agent.
    pub fn stateless() -> Self {
        Self::new(AgentStorageScope::unavailable().bind(String::new()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentStorageError {
    #[error("agent storage is unavailable in this tool runtime")]
    Unavailable,
    #[error("failed to encode agent storage object: {0}")]
    Encode(String),
    #[error("failed to decode agent storage object: {0}")]
    Decode(String),
    #[error("agent storage backend failed: {0}")]
    Backend(String),
}

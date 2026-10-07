use alloc::borrow::{Cow, ToOwned};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Ref, RefCell, RefMut};
use core::fmt;

use barracuda_agent_persistence::{
    DurablePartError, DurableState, DurableStateCodec, PersistenceError, SchemaVersion,
    SharedPersistence, StateBlob, StateSlice,
};
use portable_atomic_util::Arc;
use serde::{Deserialize, Serialize};

use super::definition::Tool;
use super::set::{ToolName, ToolSet};

pub type ToolRegistryVersion = u64;
/// A tool group id, shared by every tool of the group.
pub(crate) type ToolGroupId = Arc<str>;

const TOOL_REGISTRY_STATE_NAME: &str = "tool_registry";

pub struct ToolRegistry {
    inner: RefCell<ToolRegistryInner>,
}

struct ToolRegistryInner {
    /// Every registered tool with its group, sorted by tool name.
    tools: Vec<RegisteredTool>,
    /// Registered group ids, sorted.
    groups: Vec<ToolGroupId>,
    state: DurableState<ToolRegistryState>,
    started: bool,
    runtime_version: ToolRegistryVersion,
}

struct RegisteredTool {
    tool: Tool,
    group_id: ToolGroupId,
    default_visibility: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
struct ToolRegistryState {
    #[serde(default)]
    overrides: BTreeMap<ToolName, bool>,
}

impl DurableStateCodec for ToolRegistryState {
    const SCHEMA_VERSION: SchemaVersion = 1;

    fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
        Ok(StateBlob {
            bytes: Cow::Owned(serde_json::to_vec(self).map_err(DurablePartError::encode)?),
        })
    }

    fn decode_state(
        schema_version: SchemaVersion,
        state: StateSlice<'_>,
    ) -> Result<Self, DurablePartError> {
        if schema_version != Self::SCHEMA_VERSION {
            return Err(DurablePartError::InvalidState(
                "unsupported tool registry state schema",
            ));
        }
        serde_json::from_slice(state.bytes).map_err(DurablePartError::decode)
    }
}

impl ToolRegistryInner {
    fn tool_index(&self, name: &str) -> Result<usize, usize> {
        self.tools
            .binary_search_by(|registered| registered.tool.name().cmp(name))
    }

    fn contains_tool(&self, name: &str) -> bool {
        self.tool_index(name).is_ok()
    }

    fn contains_group(&self, id: &str) -> bool {
        self.groups
            .binary_search_by(|group| (**group).cmp(id))
            .is_ok()
    }

    /// Registers a validated group whose id and tool names are all unused.
    fn register_group(&mut self, group: ToolGroup) {
        let (id, default_visibility, group_tools) = group.into_parts();
        self.tools.reserve_exact(group_tools.len());
        for tool in group_tools {
            let (Err(index) | Ok(index)) = self.tool_index(tool.name());
            self.tools.insert(
                index,
                RegisteredTool {
                    tool,
                    group_id: Arc::clone(&id),
                    default_visibility,
                },
            );
        }
        let (Err(index) | Ok(index)) = self.groups.binary_search(&id);
        self.groups.insert(index, id);
        self.bump_runtime_version();
    }

    fn set_tool_enabled(&mut self, name: &str, enabled: bool) {
        if self.state.get().overrides.get(name).copied() == Some(enabled) {
            return;
        }
        self.state
            .get_mut()
            .overrides
            .insert(name.to_owned(), enabled);
        self.bump_runtime_version();
    }

    fn set_started(&mut self, started: bool) {
        if self.started == started {
            return;
        }
        self.started = started;
        self.bump_runtime_version();
    }

    fn bump_runtime_version(&mut self) {
        self.runtime_version = self.runtime_version.saturating_add(1);
    }

    fn tool_projection(&self) -> ToolProjection {
        if !self.started {
            return ToolProjection {
                registry_version: self.runtime_version,
                tools: Vec::new(),
            };
        }
        let state = self.state.get();
        let tools = self
            .tools
            .iter()
            .filter(|registered| {
                state.overrides.get(registered.tool.name()).copied() != Some(false)
            })
            .map(|registered| ToolProjectionEntry {
                group_id: Arc::clone(&registered.group_id),
                default_visibility: registered.default_visibility,
                tool: registered.tool.clone(),
            })
            .collect();
        ToolProjection {
            registry_version: self.runtime_version,
            tools,
        }
    }
}

pub(super) struct ToolProjection {
    pub registry_version: ToolRegistryVersion,
    pub tools: Vec<ToolProjectionEntry>,
}

/// One projected tool, in tool-name order; its name is the tool's own.
pub(super) struct ToolProjectionEntry {
    pub group_id: ToolGroupId,
    pub default_visibility: bool,
    pub tool: Tool,
}

pub struct ToolGroup {
    pub(crate) id: ToolGroupId,
    pub(crate) default_visibility: bool,
    pub(crate) tools: Vec<Tool>,
}

impl ToolGroup {
    pub fn new(
        id: impl Into<ToolGroupId>,
        default_visibility: bool,
        tools: impl IntoIterator<Item = Tool>,
    ) -> Self {
        Self {
            id: id.into(),
            default_visibility,
            tools: tools.into_iter().collect(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn into_parts(self) -> (ToolGroupId, bool, Vec<Tool>) {
        (self.id, self.default_visibility, self.tools)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolRegistryError {
    #[error("tool already exists: {0}")]
    AlreadyExists(ToolName),
    #[error("tool group already exists: {0}")]
    GroupAlreadyExists(String),
    #[error("tool not found: {0}")]
    NotFound(ToolName),
    #[error("invalid tool: {0}")]
    InvalidTool(ToolName),
    #[error("tool group and tool names must be distinct: {0}")]
    AmbiguousName(String),
    #[error("invalid tool group: {0}")]
    InvalidGroup(String),
}

/// Shared registry handle that creates per-agent [`ToolSet`] projections.
pub trait ToolSetSource {
    /// Create one per-agent tool projection over the shared registry.
    fn tool_set(&self) -> ToolSet;

    /// Create one per-agent tool projection governed by a firmware-baked
    /// blacklist. Entries match exact tool-group ids or exact tool names.
    fn tool_set_with_blacklist(&self, blacklist: &'static [&'static str]) -> ToolSet;
}

impl ToolSetSource for Arc<ToolRegistry> {
    fn tool_set(&self) -> ToolSet {
        ToolSet::from_registry(Arc::clone(self), &[])
    }

    fn tool_set_with_blacklist(&self, blacklist: &'static [&'static str]) -> ToolSet {
        ToolSet::from_registry(Arc::clone(self), blacklist)
    }
}

impl ToolRegistry {
    /// Load and register the durable state owned by this registry.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError`] when the state cannot be loaded or
    /// registered.
    pub async fn new(persistence: SharedPersistence) -> Result<Self, PersistenceError> {
        let entry = persistence.singleton::<ToolRegistryState>(TOOL_REGISTRY_STATE_NAME)?;
        let state = DurableState::new(entry.load().await?.unwrap_or_default());
        entry.register(&state)?;
        Ok(Self::from_state(state))
    }

    fn from_state(state: DurableState<ToolRegistryState>) -> Self {
        Self {
            inner: RefCell::new(ToolRegistryInner {
                tools: Vec::new(),
                groups: Vec::new(),
                state,
                started: false,
                runtime_version: 0,
            }),
        }
    }

    pub fn register_group(&self, group: ToolGroup) -> Result<(), ToolRegistryError> {
        let mut inner = self.write_state();
        if group.id.is_empty() || group.tools.is_empty() {
            return Err(ToolRegistryError::InvalidGroup(
                group.id.as_ref().to_owned(),
            ));
        }
        if inner.contains_group(&group.id) {
            return Err(ToolRegistryError::GroupAlreadyExists(
                group.id.as_ref().to_owned(),
            ));
        }
        if inner.contains_tool(&group.id) {
            return Err(ToolRegistryError::AmbiguousName(
                group.id.as_ref().to_owned(),
            ));
        }
        let mut names = BTreeSet::new();
        for tool in &group.tools {
            let name = tool.name();
            if name.is_empty() {
                return Err(ToolRegistryError::InvalidTool(name.to_owned()));
            }
            if inner.contains_group(name) || name == &*group.id {
                return Err(ToolRegistryError::AmbiguousName(name.to_owned()));
            }
            if inner.contains_tool(name) || !names.insert(name) {
                return Err(ToolRegistryError::AlreadyExists(name.to_owned()));
            }
        }

        inner.register_group(group);
        Ok(())
    }

    pub fn enable(&self, name: &str) -> Result<(), ToolRegistryError> {
        let mut inner = self.write_state();
        if !inner.contains_tool(name) {
            return Err(ToolRegistryError::NotFound(name.to_owned()));
        }

        inner.set_tool_enabled(name, true);
        Ok(())
    }

    pub fn disable(&self, name: &str) -> Result<(), ToolRegistryError> {
        let mut inner = self.write_state();
        if !inner.contains_tool(name) {
            return Err(ToolRegistryError::NotFound(name.to_owned()));
        }

        inner.set_tool_enabled(name, false);
        Ok(())
    }

    pub fn start_all(&self) -> Result<(), ToolRegistryError> {
        let mut inner = self.write_state();
        inner.set_started(true);
        Ok(())
    }

    pub fn stop_all(&self) -> Result<(), ToolRegistryError> {
        let mut inner = self.write_state();
        inner.set_started(false);
        Ok(())
    }

    pub fn tool_version(&self) -> ToolRegistryVersion {
        self.read_state().runtime_version
    }

    pub(super) fn contains_group(&self, id: &str) -> bool {
        self.read_state().contains_group(id)
    }

    pub(super) fn contains_tool(&self, name: &str) -> bool {
        self.read_state().contains_tool(name)
    }

    pub(super) fn tool_projection(&self) -> ToolProjection {
        self.read_state().tool_projection()
    }

    fn read_state(&self) -> Ref<'_, ToolRegistryInner> {
        self.inner.borrow()
    }

    fn write_state(&self) -> RefMut<'_, ToolRegistryInner> {
        self.inner.borrow_mut()
    }
}

impl fmt::Debug for ToolRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.read_state();
        let override_count = inner.state.get().overrides.len();
        formatter
            .debug_struct("ToolRegistry")
            .field("tools", &inner.tools.len())
            .field("groups", &inner.groups.len())
            .field("started", &inner.started)
            .field("runtime_version", &inner.runtime_version)
            .field("overrides", &override_count)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_contains_only_explicit_overrides() -> anyhow::Result<()> {
        let mut state = ToolRegistryState::default();
        state.overrides.insert("echo".to_owned(), false);

        let encoded = state.encode_state()?.into_owned();
        let payload: serde_json::Value = serde_json::from_slice(&encoded.bytes)?;

        assert_eq!(payload, serde_json::json!({"overrides": {"echo": false}}));
        Ok(())
    }
}

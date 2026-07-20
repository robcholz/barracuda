use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use claw_persistence::{
    DurablePartError, DurableState, DurableStateCodec, SchemaVersion, StateBlob, StateSlice,
};
use serde::{Deserialize, Serialize};

use super::set::{ToolName, ToolSet};
use super::tool::Tool;

pub type ToolRegistryVersion = u64;
pub type ToolGroupId = String;

pub struct ToolRegistry {
    inner: RwLock<ToolRegistryInner>,
}

struct ToolRegistryInner {
    tools: HashMap<ToolName, Tool>,
    groups: HashMap<ToolGroupId, ToolGroupEntry>,
    state: DurableState<ToolRegistryState>,
    started: bool,
    runtime_version: ToolRegistryVersion,
}

#[derive(Clone)]
struct ToolGroupEntry {
    default_visibility: bool,
    tools: Vec<ToolName>,
}

#[doc(hidden)]
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ToolRegistryState {
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
    fn register_group(&mut self, group: ToolGroup) {
        let default_visibility = group.default_visibility;
        let mut names = Vec::with_capacity(group.tools.len());

        for tool in group.tools {
            let name = tool.name().to_owned();
            self.tools.insert(name.clone(), tool);
            names.push(name);
        }
        self.groups.insert(
            group.id,
            ToolGroupEntry {
                default_visibility,
                tools: names,
            },
        );
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
        // Reverse index each tool to its owning group once, rather than
        // rescanning every group per tool.
        let mut group_of: HashMap<&ToolName, (&ToolGroupId, bool)> = HashMap::new();
        for (group_id, group) in &self.groups {
            for tool_name in &group.tools {
                group_of.insert(tool_name, (group_id, group.default_visibility));
            }
        }
        let state = self.state.get();
        let mut tools = Vec::with_capacity(self.tools.len());
        for (name, tool) in &self.tools {
            if state.overrides.get(name).copied() == Some(false) {
                continue;
            }
            let (group_id, default_visibility) = group_of
                .get(name)
                .map(|(group_id, visibility)| ((*group_id).clone(), *visibility))
                .unwrap_or_default();
            tools.push(ToolProjectionEntry {
                name: name.clone(),
                group_id,
                default_visibility,
                tool: tool.clone(),
            });
        }
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

pub(super) struct ToolProjectionEntry {
    pub name: ToolName,
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
    GroupAlreadyExists(ToolGroupId),
    #[error("tool not found: {0}")]
    NotFound(ToolName),
    #[error("invalid tool: {0}")]
    InvalidTool(ToolName),
    #[error("tool group and tool names must be distinct: {0}")]
    AmbiguousName(String),
    #[error("invalid tool group: {0}")]
    InvalidGroup(ToolGroupId),
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::from_state(DurableState::new(ToolRegistryState::default()))
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    #[doc(hidden)]
    pub fn from_state(state: DurableState<ToolRegistryState>) -> Self {
        Self {
            inner: RwLock::new(ToolRegistryInner {
                tools: HashMap::new(),
                groups: HashMap::new(),
                state,
                started: false,
                runtime_version: 0,
            }),
        }
    }

    pub fn register_group(&self, group: ToolGroup) -> Result<(), ToolRegistryError> {
        let mut inner = self.write_state();
        if group.id.is_empty() || group.tools.is_empty() {
            return Err(ToolRegistryError::InvalidGroup(group.id));
        }
        if inner.groups.contains_key(&group.id) {
            return Err(ToolRegistryError::GroupAlreadyExists(group.id));
        }
        if inner.tools.contains_key(&group.id) {
            return Err(ToolRegistryError::AmbiguousName(group.id));
        }
        let mut names = HashSet::with_capacity(group.tools.len());
        for tool in &group.tools {
            let name = tool.name();
            if name.is_empty() {
                return Err(ToolRegistryError::InvalidTool(name.to_owned()));
            }
            if inner.groups.contains_key(name) || name == group.id.as_str() {
                return Err(ToolRegistryError::AmbiguousName(name.to_owned()));
            }
            if inner.tools.contains_key(name) || !names.insert(name) {
                return Err(ToolRegistryError::AlreadyExists(name.to_owned()));
            }
        }

        inner.register_group(group);
        Ok(())
    }

    pub fn enable(&self, name: &str) -> Result<(), ToolRegistryError> {
        let mut inner = self.write_state();
        if !inner.tools.contains_key(name) {
            return Err(ToolRegistryError::NotFound(name.to_owned()));
        }

        inner.set_tool_enabled(name, true);
        Ok(())
    }

    pub fn disable(&self, name: &str) -> Result<(), ToolRegistryError> {
        let mut inner = self.write_state();
        if !inner.tools.contains_key(name) {
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

    pub fn tool_set(self: &Arc<Self>) -> ToolSet {
        ToolSet::new(self.clone(), &[])
    }

    /// Create one per-agent tool projection governed by a firmware-baked
    /// blacklist. Entries match exact tool-group ids or exact tool names.
    pub fn tool_set_with_blacklist(
        self: &Arc<Self>,
        blacklist: &'static [&'static str],
    ) -> ToolSet {
        ToolSet::new(self.clone(), blacklist)
    }

    pub fn tool_version(&self) -> ToolRegistryVersion {
        self.read_state().runtime_version
    }

    pub(super) fn contains_group(&self, id: &str) -> bool {
        self.read_state().groups.contains_key(id)
    }

    pub(super) fn contains_tool(&self, name: &str) -> bool {
        self.read_state().tools.contains_key(name)
    }

    pub(super) fn tool_projection(&self) -> ToolProjection {
        self.read_state().tool_projection()
    }

    fn read_state(&self) -> RwLockReadGuard<'_, ToolRegistryInner> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_state(&self) -> RwLockWriteGuard<'_, ToolRegistryInner> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
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

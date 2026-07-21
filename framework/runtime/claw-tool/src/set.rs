use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use claw_permission::Action;
use serde::Serialize;

use super::registry::{ToolGroup, ToolProjection, ToolRegistry, ToolRegistryVersion};
use super::tool::{Tool, ToolError, ToolInvocation, ToolOutput, ToolResult};

pub type ToolName = String;

const NO_SCHEMAS: &str = "no schemas";
const NO_TOOL_CONTEXT: &str = "no tool context";
const NO_EXTRA_TOOL_CONTEXT: &str = "no extra tool context";

#[derive(Debug, Default, PartialEq, Eq)]
struct ToolSetCache {
    schemas_json: Option<String>,
    tool_context: Option<String>,
    extra_tool_context: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolSource {
    Registry,
    Local,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolState {
    Enabled,
    Disabled,
    TemporarilyEnabled,
    TemporarilyDisabled,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ToolSetError {
    #[error("tool already exists: {0}")]
    AlreadyExists(ToolName),
    #[error("tool not found: {0}")]
    NotFound(ToolName),
    #[error("tool group already exists: {0}")]
    GroupAlreadyExists(String),
    #[error("invalid tool group: {0}")]
    InvalidGroup(String),
    #[error("invalid tool: {0}")]
    InvalidTool(ToolName),
    #[error("tool group and tool names must be distinct: {0}")]
    AmbiguousName(String),
}

pub struct ToolSet {
    registry: Arc<ToolRegistry>,
    blacklist: &'static [&'static str],
    local_group_ids: HashSet<String>,
    local_tool_names: HashSet<ToolName>,
    tools: HashMap<ToolName, Tool>,
    state: ToolSetState,
    cache: ToolSetCache,
    discovery: Arc<Mutex<ToolDiscovery>>,
    registry_projection_ready: bool,
    should_rebuild_temporary_tool: bool,
    should_rebuild_tool: bool,
}

#[derive(Default)]
struct ToolSetState {
    registry_version: ToolRegistryVersion,
    tools: BTreeMap<ToolName, ToolSetEntryState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolSetEntryState {
    source: ToolSource,
    state: ToolState,
    group_id: Option<String>,
    default_visibility: bool,
}

impl ToolSetEntryState {
    /// A registered-but-hidden tool the model can reveal with `tool_load`:
    /// outside the default surface and currently disabled.
    fn is_loadable(&self) -> bool {
        !self.default_visibility && self.state == ToolState::Disabled
    }
}

/// Bridge between the model-facing discovery tools (`tool_search` / `tool_load`)
/// and the [`ToolSet`] that owns visibility.
///
/// Tool handlers only have `&self`, so they cannot flip tool state directly:
/// `tool_search` reads the `catalog` the set refreshes whenever its projection
/// changes, and `tool_load` appends to `pending_loads`, which the set drains on
/// the next [`ToolSet::begin`].
#[derive(Default)]
struct ToolDiscovery {
    /// Loadable groups, grouped for `tool_search` output.
    catalog: Vec<ToolGroupCatalog>,
    /// Group ids `tool_load` asked to reveal, not yet applied.
    pending_loads: Vec<ToolName>,
}

/// Cloneable handle the discovery tools hold to reach their owning [`ToolSet`].
#[derive(Clone)]
pub struct ToolDiscoveryHandle {
    inner: Arc<Mutex<ToolDiscovery>>,
}

impl ToolDiscoveryHandle {
    /// Snapshot of the loadable (registered-but-hidden) tool groups, for
    /// `tool_search` to surface. Never includes tool schemas.
    pub fn catalog(&self) -> Vec<ToolGroupCatalog> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .catalog
            .clone()
    }

    /// Request that `group_id`'s tools be enabled on the next tick. Returns
    /// whether the group is currently loadable; a no-op for an unknown or
    /// already-queued group.
    pub fn request_load(&self, group_id: impl Into<String>) -> bool {
        let group_id = group_id.into();
        let mut discovery = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let loadable = discovery.catalog.iter().any(|group| group.id == group_id);
        if loadable && !discovery.pending_loads.contains(&group_id) {
            discovery.pending_loads.push(group_id);
        }
        loadable
    }
}

/// One loadable tool group as surfaced by `tool_search`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ToolGroupCatalog {
    pub id: String,
    pub tools: Vec<ToolCatalogEntry>,
}

/// One hidden tool inside a [`ToolGroupCatalog`] — name and short description,
/// never a schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ToolCatalogEntry {
    pub name: ToolName,
    pub description: String,
}

impl ToolSet {
    pub(super) fn new(registry: Arc<ToolRegistry>, blacklist: &'static [&'static str]) -> Self {
        Self {
            registry,
            blacklist,
            local_group_ids: HashSet::new(),
            local_tool_names: HashSet::new(),
            tools: HashMap::new(),
            state: ToolSetState::default(),
            cache: ToolSetCache::default(),
            discovery: Arc::new(Mutex::new(ToolDiscovery::default())),
            registry_projection_ready: false,
            should_rebuild_temporary_tool: false,
            should_rebuild_tool: false,
        }
    }

    /// Handle onto the discovery bridge, for building the `tool_search` /
    /// `tool_load` tools that read this set's loadable catalog and queue loads.
    pub fn discovery(&self) -> ToolDiscoveryHandle {
        ToolDiscoveryHandle {
            inner: Arc::clone(&self.discovery),
        }
    }

    pub fn add_group(&mut self, group: ToolGroup) -> Result<(), ToolSetError> {
        let (group_id, default_visibility, tools) = group.into_parts();
        if group_id.is_empty() || tools.is_empty() {
            return Err(ToolSetError::InvalidGroup(group_id));
        }
        if self.local_group_ids.contains(&group_id) || self.registry.contains_group(&group_id) {
            return Err(ToolSetError::GroupAlreadyExists(group_id));
        }
        if self.local_tool_names.contains(&group_id) || self.registry.contains_tool(&group_id) {
            return Err(ToolSetError::AmbiguousName(group_id));
        }
        let mut names = HashSet::with_capacity(tools.len());
        for tool in &tools {
            let name = tool.name();
            if name.is_empty() {
                return Err(ToolSetError::InvalidTool(name.to_owned()));
            }
            if name == group_id.as_str()
                || self.local_group_ids.contains(name)
                || self.registry.contains_group(name)
            {
                return Err(ToolSetError::AmbiguousName(name.to_owned()));
            }
            if self.local_tool_names.contains(name)
                || self.registry.contains_tool(name)
                || !names.insert(name.to_owned())
            {
                return Err(ToolSetError::AlreadyExists(name.to_owned()));
            }
        }
        self.local_group_ids.insert(group_id.clone());
        self.local_tool_names.extend(names);

        let group_blacklisted = self.blacklist.contains(&group_id.as_str());
        let mut changed = false;
        for tool in tools {
            let name = tool.name().to_owned();
            if group_blacklisted || self.blacklist.contains(&name.as_str()) {
                continue;
            }
            self.tools.insert(name.clone(), tool);
            self.state.tools.insert(
                name,
                ToolSetEntryState {
                    source: ToolSource::Local,
                    state: if default_visibility {
                        ToolState::Enabled
                    } else {
                        ToolState::Disabled
                    },
                    group_id: Some(group_id.clone()),
                    default_visibility,
                },
            );
            changed = true;
        }
        if changed {
            self.should_rebuild_tool = true;
        }
        Ok(())
    }

    pub fn enable_tool(&mut self, name: ToolName) -> Result<(), ToolSetError> {
        let Some(entry) = self.state.tools.get(&name).cloned() else {
            return Err(ToolSetError::NotFound(name));
        };
        let changed = entry.state != ToolState::Enabled;
        match entry.state {
            ToolState::Enabled => {}
            ToolState::Disabled => self.should_rebuild_tool = true,
            ToolState::TemporarilyEnabled | ToolState::TemporarilyDisabled => {
                self.should_rebuild_tool = true;
                self.should_rebuild_temporary_tool = true;
            }
        }
        if changed {
            if let Some(entry) = self.state.tools.get_mut(&name) {
                entry.state = ToolState::Enabled;
            }
        }
        Ok(())
    }

    pub fn disable_tool(&mut self, name: ToolName) -> Result<(), ToolSetError> {
        let Some(entry) = self.state.tools.get(&name).cloned() else {
            return Err(ToolSetError::NotFound(name));
        };
        let changed = entry.state != ToolState::Disabled;
        match entry.state {
            ToolState::Disabled => {}
            ToolState::Enabled => self.should_rebuild_tool = true,
            ToolState::TemporarilyEnabled | ToolState::TemporarilyDisabled => {
                self.should_rebuild_tool = true;
                self.should_rebuild_temporary_tool = true;
            }
        }
        if changed {
            if let Some(entry) = self.state.tools.get_mut(&name) {
                entry.state = ToolState::Disabled;
            }
        }
        Ok(())
    }

    pub fn temporarily_enable_tool(&mut self, name: ToolName) -> Result<(), ToolSetError> {
        let Some(entry) = self.state.tools.get(&name).cloned() else {
            return Err(ToolSetError::NotFound(name));
        };
        let next = match entry.state {
            ToolState::Disabled => {
                self.should_rebuild_temporary_tool = true;
                Some(ToolState::TemporarilyEnabled)
            }
            ToolState::TemporarilyDisabled => {
                self.should_rebuild_temporary_tool = true;
                Some(ToolState::Enabled)
            }
            ToolState::Enabled | ToolState::TemporarilyEnabled => None,
        };
        if let Some(next) = next {
            if let Some(entry) = self.state.tools.get_mut(&name) {
                entry.state = next;
            }
        }
        Ok(())
    }

    pub fn temporarily_disable_tool(&mut self, name: ToolName) -> Result<(), ToolSetError> {
        let Some(entry) = self.state.tools.get(&name).cloned() else {
            return Err(ToolSetError::NotFound(name));
        };
        let next = match entry.state {
            ToolState::Enabled => {
                self.should_rebuild_temporary_tool = true;
                Some(ToolState::TemporarilyDisabled)
            }
            ToolState::TemporarilyEnabled => {
                self.should_rebuild_temporary_tool = true;
                Some(ToolState::Disabled)
            }
            ToolState::Disabled | ToolState::TemporarilyDisabled => None,
        };
        if let Some(next) = next {
            if let Some(entry) = self.state.tools.get_mut(&name) {
                entry.state = next;
            }
        }
        Ok(())
    }

    /// Reveal the tool groups `tool_load` requested since the last projection.
    ///
    /// Loaded tools follow the same path as [`enable_tool`](Self::enable_tool),
    /// so a group stays loaded for the lifetime of this `ToolSet`. ToolSet
    /// runtime state is not restored after a process restart.
    fn apply_pending_tool_loads(&mut self) {
        let pending: HashSet<ToolName> = {
            let mut discovery = self
                .discovery
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            core::mem::take(&mut discovery.pending_loads)
                .into_iter()
                .collect()
        };
        if pending.is_empty() {
            return;
        }
        let to_enable: Vec<ToolName> = self
            .state
            .tools
            .iter()
            .filter(|(_, entry)| {
                entry.is_loadable()
                    && entry
                        .group_id
                        .as_ref()
                        .is_some_and(|group_id| pending.contains(group_id))
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in to_enable {
            // Only fails for an unknown tool; every name came from state.
            let _ = self.enable_tool(name);
        }
    }

    pub fn clear_temporary_tools(&mut self) {
        let changes: Vec<_> = self
            .state
            .tools
            .iter()
            .filter_map(|(name, entry)| match entry.state {
                ToolState::TemporarilyEnabled => Some((name.clone(), ToolState::Disabled)),
                ToolState::TemporarilyDisabled => Some((name.clone(), ToolState::Enabled)),
                ToolState::Enabled | ToolState::Disabled => None,
            })
            .collect();
        if changes.is_empty() {
            return;
        }
        let state = &mut self.state;
        for (name, next) in changes {
            if let Some(entry) = state.tools.get_mut(&name) {
                entry.state = next;
            }
        }
        self.should_rebuild_temporary_tool = true;
    }

    pub fn loaded_groups(&self) -> Vec<String> {
        self.state
            .tools
            .values()
            .filter(|entry| !entry.default_visibility && entry.state == ToolState::Enabled)
            .filter_map(|entry| entry.group_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[doc(hidden)]
    pub fn resume_detail(mut loaded_groups: Vec<String>) -> Option<String> {
        loaded_groups.sort_unstable();
        loaded_groups.dedup();
        (!loaded_groups.is_empty()).then(|| {
            format!(
                "previously loaded tool groups: {}",
                loaded_groups.join(", ")
            )
        })
    }

    pub fn begin(&mut self) -> Result<ToolSetHandle<'_>, ToolSetError> {
        self.apply_pending_tool_loads();
        let registry_version = self.registry.tool_version();
        if !self.registry_projection_ready || self.state.registry_version != registry_version {
            self.rebuild()?;
        } else if self.should_rebuild_tool {
            self.rebuild_cache();
        } else if self.should_rebuild_temporary_tool {
            self.rebuild_extra_tool_context();
        }
        Ok(ToolSetHandle {
            tools: &self.tools,
            states: &self.state.tools,
            cache: &self.cache,
        })
    }

    fn rebuild(&mut self) -> Result<(), ToolSetError> {
        let projection = self.registry.tool_projection();
        self.validate_registry_namespace(&projection)?;
        let registry_names = projection
            .tools
            .iter()
            .filter(|entry| !self.is_blacklisted(&entry.group_id, &entry.name))
            .map(|entry| entry.name.clone())
            .collect::<HashSet<_>>();

        self.tools.retain(|name, _| {
            self.state
                .tools
                .get(name)
                .is_some_and(|entry| entry.source == ToolSource::Local)
                || registry_names.contains(name)
        });

        let mut tool_states = self.state.tools.clone();
        tool_states.retain(|name, entry| {
            entry.source == ToolSource::Local || registry_names.contains(name)
        });

        for entry in projection.tools {
            if self.is_blacklisted(&entry.group_id, &entry.name) {
                continue;
            }
            let carried_state =
                tool_states
                    .get(&entry.name)
                    .and_then(|tool_state| match tool_state.source {
                        ToolSource::Registry => Some(tool_state.state),
                        ToolSource::Local => {
                            tracing::trace!(
                                tool = entry.name.as_str(),
                                "registry tool overrides local tool"
                            );
                            None
                        }
                    });
            let state = carried_state.unwrap_or(if entry.default_visibility {
                ToolState::Enabled
            } else {
                ToolState::Disabled
            });
            self.tools.insert(entry.name.clone(), entry.tool);
            tool_states.insert(
                entry.name,
                ToolSetEntryState {
                    source: ToolSource::Registry,
                    state,
                    group_id: Some(entry.group_id),
                    default_visibility: entry.default_visibility,
                },
            );
        }

        if self.state.registry_version != projection.registry_version
            || self.state.tools != tool_states
        {
            self.state = ToolSetState {
                registry_version: projection.registry_version,
                tools: tool_states,
            };
        }
        self.rebuild_cache();
        self.registry_projection_ready = true;
        Ok(())
    }

    fn is_blacklisted(&self, group_id: &str, tool_name: &str) -> bool {
        self.blacklist.contains(&group_id) || self.blacklist.contains(&tool_name)
    }

    fn validate_registry_namespace(&self, projection: &ToolProjection) -> Result<(), ToolSetError> {
        for entry in &projection.tools {
            if self.local_group_ids.contains(&entry.group_id) {
                return Err(ToolSetError::GroupAlreadyExists(entry.group_id.clone()));
            }
            if self.local_tool_names.contains(&entry.name) {
                return Err(ToolSetError::AlreadyExists(entry.name.clone()));
            }
            if self.local_group_ids.contains(&entry.name) {
                return Err(ToolSetError::AmbiguousName(entry.name.clone()));
            }
            if self.local_tool_names.contains(&entry.group_id) {
                return Err(ToolSetError::AmbiguousName(entry.group_id.clone()));
            }
        }
        Ok(())
    }

    fn rebuild_cache(&mut self) {
        self.render_schemas_json();
        self.render_tool_context();
        self.rebuild_extra_tool_context();
        self.refresh_discovery_catalog();
        self.should_rebuild_tool = false;
    }

    fn rebuild_extra_tool_context(&mut self) {
        self.render_extra_tool_context();
        self.should_rebuild_temporary_tool = false;
    }

    fn refresh_discovery_catalog(&self) {
        let mut groups = BTreeMap::<String, Vec<ToolCatalogEntry>>::new();
        for (name, entry) in &self.state.tools {
            if !entry.is_loadable() {
                continue;
            }
            let Some(group_id) = entry.group_id.as_ref() else {
                continue;
            };
            let Some(tool) = self.tools.get(name) else {
                continue;
            };
            groups
                .entry(group_id.clone())
                .or_default()
                .push(ToolCatalogEntry {
                    name: name.clone(),
                    description: tool_description(tool),
                });
        }
        let catalog = groups
            .into_iter()
            .map(|(id, tools)| ToolGroupCatalog { id, tools })
            .collect();
        self.discovery
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .catalog = catalog;
    }

    fn render_schemas_json(&mut self) {
        let schemas_json = self.cache.schemas_json.get_or_insert_with(String::new);
        schemas_json.clear();

        let mut has_tool = false;
        schemas_json.push('[');
        for (name, entry) in &self.state.tools {
            if !matches!(
                entry.state,
                ToolState::Enabled | ToolState::TemporarilyDisabled
            ) {
                continue;
            }
            let Some(tool) = self.tools.get(name) else {
                continue;
            };
            if has_tool {
                schemas_json.push(',');
            }
            schemas_json.push_str(tool.schema());
            has_tool = true;
        }
        if has_tool {
            schemas_json.push(']');
        } else {
            schemas_json.clear();
        }
    }

    fn render_tool_context(&mut self) {
        let tool_context = self.cache.tool_context.get_or_insert_with(String::new);
        tool_context.clear();

        for (name, entry) in &self.state.tools {
            if !matches!(
                entry.state,
                ToolState::Enabled | ToolState::TemporarilyDisabled
            ) {
                continue;
            }
            let Some(tool) = self.tools.get(name) else {
                continue;
            };
            let Some(usage) = tool.usage() else {
                continue;
            };
            if !tool_context.is_empty() {
                tool_context.push_str("\n\n");
            }
            tool_context.push_str(usage);
        }
    }

    fn render_extra_tool_context(&mut self) {
        let extra_context = self
            .cache
            .extra_tool_context
            .get_or_insert_with(String::new);
        extra_context.clear();

        for (name, entry) in &self.state.tools {
            match entry.state {
                ToolState::TemporarilyEnabled => {
                    let Some(tool) = self.tools.get(name) else {
                        continue;
                    };
                    if !extra_context.is_empty() {
                        extra_context.push_str("\n\n");
                    }
                    extra_context.push_str("Tool `");
                    extra_context.push_str(name);
                    extra_context.push_str("` is temporarily available.\n");
                    match tool.usage() {
                        Some(usage) => extra_context.push_str(usage),
                        None => extra_context.push_str(tool.schema()),
                    }
                }
                ToolState::TemporarilyDisabled => {
                    if !extra_context.is_empty() {
                        extra_context.push_str("\n\n");
                    }
                    extra_context.push_str("Tool `");
                    extra_context.push_str(name);
                    extra_context.push_str("` is temporarily unavailable.");
                }
                ToolState::Enabled | ToolState::Disabled => {}
            }
        }
    }
}

pub struct ToolSetHandle<'a> {
    tools: &'a HashMap<ToolName, Tool>,
    states: &'a BTreeMap<ToolName, ToolSetEntryState>,
    cache: &'a ToolSetCache,
}

impl<'a> ToolSetHandle<'a> {
    pub fn schemas_json(&self) -> &str {
        match self
            .cache
            .schemas_json
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            Some(schemas_json) => schemas_json,
            None => NO_SCHEMAS,
        }
    }

    pub fn tool_context(&self) -> &str {
        match self
            .cache
            .tool_context
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            Some(tool_context) => tool_context,
            None => NO_TOOL_CONTEXT,
        }
    }

    pub fn extra_tool_context(&self) -> &str {
        match self
            .cache
            .extra_tool_context
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            Some(extra_tool_context) => extra_tool_context,
            None => NO_EXTRA_TOOL_CONTEXT,
        }
    }

    pub(crate) fn classify(&self, call: &ToolInvocation<'_>) -> ToolResult<Action> {
        match (self.tools.get(call.name()), self.states.get(call.name())) {
            (Some(tool), Some(entry))
                if matches!(
                    entry.state,
                    ToolState::Enabled | ToolState::TemporarilyEnabled
                ) =>
            {
                Ok(tool.classify(call))
            }
            (_, Some(entry)) if entry.state == ToolState::TemporarilyDisabled => {
                Err(ToolError::InvokeRejected(unavailable_message(call.name())).into())
            }
            _ => Err(ToolError::NotFound(call.name().to_owned()).into()),
        }
    }

    pub async fn invoke<'call>(
        &self,
        call: &'call ToolInvocation<'call>,
    ) -> ToolResult<ToolOutput> {
        match (self.tools.get(call.name()), self.states.get(call.name())) {
            (Some(tool), Some(entry))
                if matches!(
                    entry.state,
                    ToolState::Enabled | ToolState::TemporarilyEnabled
                ) =>
            {
                tool.invoke(call).await
            }
            (_, Some(entry)) if entry.state == ToolState::TemporarilyDisabled => {
                Err(ToolError::InvokeRejected(unavailable_message(call.name())).into())
            }
            _ => Err(ToolError::NotFound(call.name().to_owned()).into()),
        }
    }
}

/// Short, schema-free description of a hidden tool for the discovery catalog:
/// its usage line if any, else the `description` from its schema.
fn tool_description(tool: &Tool) -> String {
    if let Some(usage) = tool
        .usage()
        .map(str::trim)
        .filter(|usage| !usage.is_empty())
    {
        return usage.to_owned();
    }
    serde_json::from_str::<serde_json::Value>(tool.schema())
        .ok()
        .and_then(|schema| {
            schema
                .pointer("/function/description")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn unavailable_message(name: &str) -> String {
    let mut message = String::from("tool is temporarily unavailable: ");
    message.push_str(name);
    message
}

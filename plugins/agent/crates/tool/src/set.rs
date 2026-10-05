use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use barracuda_agent_message::json::Sink;
use barracuda_agent_message::BulkText;
use barracuda_agent_permission::Action;
use portable_atomic_util::Arc;
use serde::Serialize;

use super::definition::{Tool, ToolError, ToolInvocation, ToolResult};
use super::registry::{ToolGroup, ToolGroupId, ToolProjection, ToolRegistry, ToolRegistryVersion};

pub type ToolName = String;

const NO_SCHEMAS: &str = "no schemas";
const NO_TOOL_CONTEXT: &str = "no tool context";
const NO_EXTRA_TOOL_CONTEXT: &str = "no extra tool context";

/// Model-facing renderings of the current tool surface, each held as exactly
/// sized bulk text.
#[derive(Debug, Default, PartialEq, Eq)]
struct ToolSetCache {
    static_schemas: Option<BulkText>,
    static_context: Option<BulkText>,
    deferred_context: Option<BulkText>,
    extra_tool_context: Option<BulkText>,
}

impl ToolSetCache {
    fn text(text: &Option<BulkText>) -> Option<&str> {
        text.as_ref()
            .map(BulkText::as_str)
            .filter(|text| !text.is_empty())
    }
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
    registry: Option<Arc<ToolRegistry>>,
    blacklist: &'static [&'static str],
    /// Groups added to this set, blacklisted tools included, so their ids and
    /// tool names stay reserved.
    local_groups: Vec<LocalGroup>,
    /// Every tool in the set with its visibility state, sorted by tool name.
    entries: Vec<ToolEntry>,
    /// Registry version `entries` reflects.
    registry_version: ToolRegistryVersion,
    cache: ToolSetCache,
    discovery: Arc<RefCell<ToolDiscovery>>,
    registry_projection_ready: bool,
    should_rebuild_temporary_tool: bool,
    should_rebuild_tool: bool,
}

/// One group added to a [`ToolSet`].
struct LocalGroup {
    id: ToolGroupId,
    tools: Vec<Tool>,
}

/// One tool in a [`ToolSet`] together with its visibility state, named by
/// the tool itself.
#[derive(Clone)]
struct ToolEntry {
    tool: Tool,
    source: ToolSource,
    state: ToolState,
    group_id: ToolGroupId,
    default_visibility: bool,
}

/// Finds the entry for `name` in entries sorted by tool name.
fn find_entry(entries: &[ToolEntry], name: &str) -> Result<usize, usize> {
    entries.binary_search_by(|entry| entry.tool.name().cmp(name))
}

/// Inserts `entry`, replacing any entry of the same name.
fn put_entry(entries: &mut Vec<ToolEntry>, entry: ToolEntry) {
    match find_entry(entries, entry.tool.name()) {
        Ok(index) => {
            if let Some(slot) = entries.get_mut(index) {
                *slot = entry;
            }
        }
        Err(index) => entries.insert(index, entry),
    }
}

impl ToolEntry {
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
    /// Loadable groups and their tools; `tool_search` derives names and
    /// descriptions from the tools when it runs.
    catalog: Vec<LoadableGroup>,
    /// Group ids `tool_load` asked to reveal, not yet applied.
    pending_loads: Vec<ToolName>,
}

/// Cloneable handle the discovery tools hold to reach their owning [`ToolSet`].
#[derive(Clone)]
pub struct ToolDiscoveryHandle {
    inner: Arc<RefCell<ToolDiscovery>>,
}

impl ToolDiscoveryHandle {
    /// Snapshot of the loadable (registered-but-hidden) tool groups, for
    /// `tool_search` to surface. Never includes tool schemas.
    pub fn catalog(&self) -> Vec<ToolGroupCatalog> {
        self.inner
            .borrow()
            .catalog
            .iter()
            .map(|group| ToolGroupCatalog {
                id: group.id.as_ref().to_owned(),
                tools: group
                    .tools
                    .iter()
                    .map(|tool| ToolCatalogEntry {
                        name: tool.name().to_owned(),
                        description: tool_description(tool),
                    })
                    .collect(),
            })
            .collect()
    }

    /// Request that `group_id`'s tools be enabled on the next tick. Returns
    /// whether the group is currently loadable; a no-op for an unknown or
    /// already-queued group.
    pub fn request_load(&self, group_id: impl Into<String>) -> bool {
        let group_id = group_id.into();
        let mut discovery = self.inner.borrow_mut();
        let loadable = discovery.catalog.iter().any(|group| *group.id == *group_id);
        if loadable && !discovery.pending_loads.contains(&group_id) {
            discovery.pending_loads.push(group_id);
        }
        loadable
    }
}

/// One loadable tool group held by the discovery bridge.
struct LoadableGroup {
    id: ToolGroupId,
    tools: Vec<Tool>,
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
    /// Create an isolated set containing no registry tools.
    pub fn empty() -> Self {
        Self::new(None, &[])
    }

    pub(super) fn from_registry(
        registry: Arc<ToolRegistry>,
        blacklist: &'static [&'static str],
    ) -> Self {
        Self::new(Some(registry), blacklist)
    }

    fn new(registry: Option<Arc<ToolRegistry>>, blacklist: &'static [&'static str]) -> Self {
        let registry_projection_ready = registry.is_none();
        Self {
            registry,
            blacklist,
            local_groups: Vec::new(),
            entries: Vec::new(),
            registry_version: ToolRegistryVersion::default(),
            cache: ToolSetCache::default(),
            discovery: Arc::new(RefCell::new(ToolDiscovery::default())),
            registry_projection_ready,
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
            return Err(ToolSetError::InvalidGroup(group_id.as_ref().to_owned()));
        }
        if self.has_local_group(&group_id) || self.registry_contains_group(&group_id) {
            return Err(ToolSetError::GroupAlreadyExists(
                group_id.as_ref().to_owned(),
            ));
        }
        if self.has_local_tool(&group_id) || self.registry_contains_tool(&group_id) {
            return Err(ToolSetError::AmbiguousName(group_id.as_ref().to_owned()));
        }
        for (index, tool) in tools.iter().enumerate() {
            let name = tool.name();
            if name.is_empty() {
                return Err(ToolSetError::InvalidTool(name.to_owned()));
            }
            if name == &*group_id
                || self.has_local_group(name)
                || self.registry_contains_group(name)
            {
                return Err(ToolSetError::AmbiguousName(name.to_owned()));
            }
            let repeated = tools
                .get(..index)
                .is_some_and(|earlier| earlier.iter().any(|earlier| earlier.name() == name));
            if repeated || self.has_local_tool(name) || self.registry_contains_tool(name) {
                return Err(ToolSetError::AlreadyExists(name.to_owned()));
            }
        }

        let group_blacklisted = self.blacklist.contains(&&*group_id);
        let mut changed = false;
        self.entries.reserve_exact(tools.len());
        for tool in &tools {
            if group_blacklisted || self.blacklist.contains(&tool.name()) {
                continue;
            }
            put_entry(
                &mut self.entries,
                ToolEntry {
                    tool: tool.clone(),
                    source: ToolSource::Local,
                    state: if default_visibility {
                        ToolState::Enabled
                    } else {
                        ToolState::Disabled
                    },
                    group_id: Arc::clone(&group_id),
                    default_visibility,
                },
            );
            changed = true;
        }
        self.local_groups.push(LocalGroup {
            id: group_id,
            tools,
        });
        if changed {
            self.should_rebuild_tool = true;
        }
        Ok(())
    }

    pub fn enable_tool(&mut self, name: ToolName) -> Result<(), ToolSetError> {
        let Some(current) = self.entry(&name).map(|entry| entry.state) else {
            return Err(ToolSetError::NotFound(name));
        };
        let changed = current != ToolState::Enabled;
        match current {
            ToolState::Enabled => {}
            ToolState::Disabled => self.should_rebuild_tool = true,
            ToolState::TemporarilyEnabled | ToolState::TemporarilyDisabled => {
                self.should_rebuild_tool = true;
                self.should_rebuild_temporary_tool = true;
            }
        }
        if changed {
            if let Some(entry) = self.entry_mut(&name) {
                entry.state = ToolState::Enabled;
            }
        }
        Ok(())
    }

    pub fn disable_tool(&mut self, name: ToolName) -> Result<(), ToolSetError> {
        let Some(current) = self.entry(&name).map(|entry| entry.state) else {
            return Err(ToolSetError::NotFound(name));
        };
        let changed = current != ToolState::Disabled;
        match current {
            ToolState::Disabled => {}
            ToolState::Enabled => self.should_rebuild_tool = true,
            ToolState::TemporarilyEnabled | ToolState::TemporarilyDisabled => {
                self.should_rebuild_tool = true;
                self.should_rebuild_temporary_tool = true;
            }
        }
        if changed {
            if let Some(entry) = self.entry_mut(&name) {
                entry.state = ToolState::Disabled;
            }
        }
        Ok(())
    }

    pub fn temporarily_enable_tool(&mut self, name: ToolName) -> Result<(), ToolSetError> {
        let Some(current) = self.entry(&name).map(|entry| entry.state) else {
            return Err(ToolSetError::NotFound(name));
        };
        let next = match current {
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
            if let Some(entry) = self.entry_mut(&name) {
                entry.state = next;
            }
        }
        Ok(())
    }

    pub fn temporarily_disable_tool(&mut self, name: ToolName) -> Result<(), ToolSetError> {
        let Some(current) = self.entry(&name).map(|entry| entry.state) else {
            return Err(ToolSetError::NotFound(name));
        };
        let next = match current {
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
            if let Some(entry) = self.entry_mut(&name) {
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
        let pending = core::mem::take(&mut self.discovery.borrow_mut().pending_loads);
        for entry in &mut self.entries {
            if entry.is_loadable() && pending.iter().any(|group_id| **group_id == *entry.group_id) {
                entry.state = ToolState::Enabled;
                self.should_rebuild_tool = true;
            }
        }
    }

    pub fn clear_temporary_tools(&mut self) {
        for entry in &mut self.entries {
            let next = match entry.state {
                ToolState::TemporarilyEnabled => ToolState::Disabled,
                ToolState::TemporarilyDisabled => ToolState::Enabled,
                ToolState::Enabled | ToolState::Disabled => continue,
            };
            entry.state = next;
            self.should_rebuild_temporary_tool = true;
        }
    }

    pub fn begin(&mut self) -> Result<ToolSetHandle<'_>, ToolSetError> {
        self.apply_pending_tool_loads();
        let should_rebuild_registry = self.registry.as_ref().is_some_and(|registry| {
            !self.registry_projection_ready || self.registry_version != registry.tool_version()
        });
        if should_rebuild_registry {
            self.rebuild_registry()?;
        } else if self.should_rebuild_tool {
            self.rebuild_cache();
        } else if self.should_rebuild_temporary_tool {
            self.rebuild_extra_tool_context();
        }
        Ok(ToolSetHandle {
            entries: &self.entries,
            cache: &self.cache,
        })
    }

    fn rebuild_registry(&mut self) -> Result<(), ToolSetError> {
        let Some(registry) = self.registry.as_ref() else {
            return Ok(());
        };
        let projection = registry.tool_projection();
        self.validate_registry_namespace(&projection)?;
        let mut entries = core::mem::take(&mut self.entries);
        // Projected tools are sorted by name, like the entries.
        entries.retain(|existing| {
            existing.source == ToolSource::Local
                || projection
                    .tools
                    .binary_search_by(|entry| entry.tool.name().cmp(existing.tool.name()))
                    .ok()
                    .and_then(|index| projection.tools.get(index))
                    .is_some_and(|entry| !self.is_blacklisted(&entry.group_id, entry.tool.name()))
        });

        for entry in projection.tools {
            let name = entry.tool.name();
            if self.is_blacklisted(&entry.group_id, name) {
                continue;
            }
            let carried_state = find_entry(&entries, name)
                .ok()
                .and_then(|index| entries.get(index))
                .and_then(|existing| match existing.source {
                    ToolSource::Registry => Some(existing.state),
                    ToolSource::Local => {
                        tracing::trace!(tool = name, "registry tool overrides local tool");
                        None
                    }
                });
            let state = carried_state.unwrap_or(if entry.default_visibility {
                ToolState::Enabled
            } else {
                ToolState::Disabled
            });
            put_entry(
                &mut entries,
                ToolEntry {
                    tool: entry.tool,
                    source: ToolSource::Registry,
                    state,
                    group_id: entry.group_id,
                    default_visibility: entry.default_visibility,
                },
            );
        }

        self.entries = entries;
        self.registry_version = projection.registry_version;
        self.rebuild_cache();
        self.registry_projection_ready = true;
        Ok(())
    }

    fn entry(&self, name: &str) -> Option<&ToolEntry> {
        find_entry(&self.entries, name)
            .ok()
            .and_then(|index| self.entries.get(index))
    }

    fn entry_mut(&mut self, name: &str) -> Option<&mut ToolEntry> {
        find_entry(&self.entries, name)
            .ok()
            .and_then(|index| self.entries.get_mut(index))
    }

    fn has_local_group(&self, id: &str) -> bool {
        self.local_groups.iter().any(|group| *group.id == *id)
    }

    fn has_local_tool(&self, name: &str) -> bool {
        self.local_groups
            .iter()
            .any(|group| group.tools.iter().any(|tool| tool.name() == name))
    }

    fn registry_contains_group(&self, id: &str) -> bool {
        self.registry
            .as_ref()
            .is_some_and(|registry| registry.contains_group(id))
    }

    fn registry_contains_tool(&self, name: &str) -> bool {
        self.registry
            .as_ref()
            .is_some_and(|registry| registry.contains_tool(name))
    }

    fn is_blacklisted(&self, group_id: &str, tool_name: &str) -> bool {
        self.blacklist.contains(&group_id) || self.blacklist.contains(&tool_name)
    }

    fn validate_registry_namespace(&self, projection: &ToolProjection) -> Result<(), ToolSetError> {
        for entry in &projection.tools {
            let name = entry.tool.name();
            if self.has_local_group(&entry.group_id) {
                return Err(ToolSetError::GroupAlreadyExists(
                    entry.group_id.as_ref().to_owned(),
                ));
            }
            if self.has_local_tool(name) {
                return Err(ToolSetError::AlreadyExists(name.to_owned()));
            }
            if self.has_local_group(name) {
                return Err(ToolSetError::AmbiguousName(name.to_owned()));
            }
            if self.has_local_tool(&entry.group_id) {
                return Err(ToolSetError::AmbiguousName(
                    entry.group_id.as_ref().to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn rebuild_cache(&mut self) {
        self.render_static_tools();
        self.render_deferred_tools();
        self.rebuild_extra_tool_context();
        self.refresh_discovery_catalog();
        self.should_rebuild_tool = false;
    }

    fn rebuild_extra_tool_context(&mut self) {
        self.render_extra_tool_context();
        self.should_rebuild_temporary_tool = false;
    }

    fn refresh_discovery_catalog(&self) {
        let mut groups = BTreeMap::<&ToolGroupId, Vec<Tool>>::new();
        for entry in self.entries.iter().filter(|entry| entry.is_loadable()) {
            groups
                .entry(&entry.group_id)
                .or_default()
                .push(entry.tool.clone());
        }
        self.discovery.borrow_mut().catalog = groups
            .into_iter()
            .map(|(id, tools)| LoadableGroup {
                id: Arc::clone(id),
                tools,
            })
            .collect();
    }

    /// Tools on the default (`true`) or deferred (`false`) surface that the
    /// model can currently see.
    fn surface(&self, default_visibility: bool) -> Vec<&Tool> {
        self.entries
            .iter()
            .filter(|entry| {
                entry.default_visibility == default_visibility
                    && matches!(
                        entry.state,
                        ToolState::Enabled | ToolState::TemporarilyDisabled
                    )
            })
            .map(|entry| &entry.tool)
            .collect()
    }

    fn render_static_tools(&mut self) {
        let (schemas, context) = {
            let tools = self.surface(true);
            (
                BulkText::encode(|sink| write_schemas(sink, &tools)),
                BulkText::encode(|sink| {
                    write_usages(sink, &tools);
                }),
            )
        };
        self.cache.static_schemas = Some(schemas);
        self.cache.static_context = Some(context);
    }

    fn render_deferred_tools(&mut self) {
        let context = {
            let tools = self.surface(false);
            BulkText::encode(|sink| {
                let wrote_usage = write_usages(sink, &tools);
                if !tools.is_empty() {
                    if wrote_usage {
                        sink.put(b"\n\n");
                    }
                    write_schemas(sink, &tools);
                }
            })
        };
        self.cache.deferred_context = Some(context);
    }

    fn render_extra_tool_context(&mut self) {
        let context = BulkText::encode(|sink| {
            let mut first = true;
            for entry in &self.entries {
                let tool = match entry.state {
                    ToolState::TemporarilyEnabled => Some(&entry.tool),
                    ToolState::TemporarilyDisabled => None,
                    ToolState::Enabled | ToolState::Disabled => continue,
                };
                if !first {
                    sink.put(b"\n\n");
                }
                first = false;
                sink.put(b"Tool `");
                sink.put(entry.tool.name().as_bytes());
                match tool {
                    Some(tool) => {
                        sink.put(b"` is temporarily available.\n");
                        sink.put(tool.usage().unwrap_or(tool.schema()).as_bytes());
                    }
                    None => sink.put(b"` is temporarily unavailable."),
                }
            }
        });
        self.cache.extra_tool_context = Some(context);
    }
}

pub struct ToolSetHandle<'a> {
    entries: &'a [ToolEntry],
    cache: &'a ToolSetCache,
}

impl<'a> ToolSetHandle<'a> {
    fn entry(&self, name: &str) -> Option<&'a ToolEntry> {
        find_entry(self.entries, name)
            .ok()
            .and_then(|index| self.entries.get(index))
    }

    /// Schemas for tools present in the default, immutable tool surface.
    pub fn static_schemas(&self) -> &str {
        ToolSetCache::text(&self.cache.static_schemas).unwrap_or(NO_SCHEMAS)
    }

    /// Usage context for tools present in the default, immutable tool surface.
    pub fn static_context(&self) -> &str {
        ToolSetCache::text(&self.cache.static_context).unwrap_or(NO_TOOL_CONTEXT)
    }

    /// Usage and schemas for hidden tools revealed through `tool_load`.
    pub fn deferred_context(&self) -> &str {
        ToolSetCache::text(&self.cache.deferred_context).unwrap_or_default()
    }

    /// Per-iteration status for temporarily enabled or disabled tools.
    pub fn reminders(&self) -> &str {
        ToolSetCache::text(&self.cache.extra_tool_context).unwrap_or(NO_EXTRA_TOOL_CONTEXT)
    }

    /// Classify one call for a caller-owned permission evaluation phase.
    pub fn classify(&self, call: &ToolInvocation) -> ToolResult<Action> {
        match self.entry(call.name()) {
            Some(entry)
                if matches!(
                    entry.state,
                    ToolState::Enabled | ToolState::TemporarilyEnabled
                ) =>
            {
                entry.tool.classify(call)
            }
            Some(entry) if entry.state == ToolState::TemporarilyDisabled => {
                Err(ToolError::InvokeRejected(unavailable_message(call.name())).into())
            }
            _ => Err(ToolError::NotFound(call.name().to_owned()).into()),
        }
    }

    pub(crate) fn runnable_tool(&self, call: &ToolInvocation) -> ToolResult<Tool> {
        match self.entry(call.name()) {
            Some(entry)
                if matches!(
                    entry.state,
                    ToolState::Enabled | ToolState::TemporarilyEnabled
                ) =>
            {
                Ok(entry.tool.clone())
            }
            Some(entry) if entry.state == ToolState::TemporarilyDisabled => {
                Err(ToolError::InvokeRejected(unavailable_message(call.name())).into())
            }
            _ => Err(ToolError::NotFound(call.name().to_owned()).into()),
        }
    }
}

/// Writes `[schema,schema,...]`, or nothing when `tools` is empty.
fn write_schemas(sink: &mut dyn Sink, tools: &[&Tool]) {
    if tools.is_empty() {
        return;
    }
    sink.put(b"[");
    for (index, tool) in tools.iter().enumerate() {
        if index > 0 {
            sink.put(b",");
        }
        sink.put(tool.schema().as_bytes());
    }
    sink.put(b"]");
}

/// Writes each tool's usage text separated by blank lines; returns whether
/// any usage was written.
fn write_usages(sink: &mut dyn Sink, tools: &[&Tool]) -> bool {
    let mut wrote = false;
    for usage in tools.iter().filter_map(|tool| tool.usage()) {
        if wrote {
            sink.put(b"\n\n");
        }
        sink.put(usage.as_bytes());
        wrote = true;
    }
    wrote
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

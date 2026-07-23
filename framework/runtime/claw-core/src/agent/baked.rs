//! Agent definitions baked into the firmware at compile time.
//!
//! [`AgentRuntimeManifest`] is consumed while assembling one Agent;
//! [`MultiagentManifest`] is consumed only by the Multiagent extension.

use std::borrow::Cow;

/// Which baked agent template to instantiate from `resources/agents/<kind>/`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AgentKind(Cow<'static, str>);

impl AgentKind {
    pub(crate) fn new(kind: String) -> Self {
        Self(Cow::Owned(kind))
    }

    pub(crate) const fn from_static(kind: &'static str) -> Self {
        Self(Cow::Borrowed(kind))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub(crate) struct AgentCatalogEntry {
    kind: AgentKind,
    description: &'static str,
    runtime: AgentRuntimeManifest,
    multiagent: MultiagentManifest,
}

impl AgentCatalogEntry {
    pub(crate) fn kind(&self) -> &AgentKind {
        &self.kind
    }

    pub(crate) fn description(&self) -> &'static str {
        self.description
    }

    pub(crate) fn runtime(&self) -> &AgentRuntimeManifest {
        &self.runtime
    }

    pub(crate) fn multiagent(&self) -> &MultiagentManifest {
        &self.multiagent
    }
}

/// Configuration needed to construct one agent in isolation.
pub(crate) struct AgentRuntimeManifest {
    retries: u32,
    tool_blacklist: &'static [&'static str],
    instructions: &'static str,
}

impl AgentRuntimeManifest {
    pub(crate) fn retries(&self) -> u32 {
        self.retries
    }

    pub(crate) fn tool_blacklist(&self) -> &'static [&'static str] {
        self.tool_blacklist
    }

    pub(crate) fn instructions(&self) -> &'static str {
        self.instructions
    }
}

/// Orchestration policy intentionally kept outside the single-agent runtime.
pub(crate) struct MultiagentManifest {
    spawn_enabled: bool,
    allowed_kinds: &'static [AgentKind],
}

impl MultiagentManifest {
    pub(crate) fn spawn_enabled(&self) -> bool {
        self.spawn_enabled
    }

    pub(crate) fn allowed_kinds(&self) -> &'static [AgentKind] {
        self.allowed_kinds
    }
}

pub(crate) fn find(kind: &AgentKind) -> Option<&'static AgentCatalogEntry> {
    entries().iter().find(|entry| entry.kind() == kind)
}

pub(crate) fn entries() -> &'static [AgentCatalogEntry] {
    ENTRIES
}

/// The sole root kind selected and verified by the manifest generator.
pub(crate) fn root_kind() -> &'static AgentKind {
    &ROOT_KIND
}

include!(concat!(env!("OUT_DIR"), "/manifests.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_root_kind_is_a_catalog_entry() {
        assert!(find(root_kind()).is_some());
    }
}

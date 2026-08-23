//! Agent definitions baked into the firmware at compile time.
//!
//! [`AgentRuntimeManifest`] is consumed while assembling one Agent; the
//! optional multiagent manifest is consumed only by the Multiagent extension.

use alloc::{borrow::Cow, string::String};
use core::fmt;

/// Which baked agent template to instantiate from `resources/agents/<kind>/`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AgentKind(Cow<'static, str>);

impl AgentKind {
    pub fn new(kind: String) -> Self {
        Self(Cow::Owned(kind))
    }

    pub const fn from_static(kind: &'static str) -> Self {
        Self(Cow::Borrowed(kind))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub struct AgentCatalogEntry {
    kind: AgentKind,
    #[cfg(feature = "multiagent")]
    description: &'static str,
    runtime: AgentRuntimeManifest,
    #[cfg(feature = "multiagent")]
    multiagent: MultiagentManifest,
}

impl AgentCatalogEntry {
    pub fn kind(&self) -> &AgentKind {
        &self.kind
    }

    #[cfg(feature = "multiagent")]
    pub fn description(&self) -> &'static str {
        self.description
    }

    pub fn runtime(&self) -> &AgentRuntimeManifest {
        &self.runtime
    }

    #[cfg(feature = "multiagent")]
    pub fn multiagent(&self) -> &MultiagentManifest {
        &self.multiagent
    }
}

/// Configuration needed to construct one agent in isolation.
pub struct AgentRuntimeManifest {
    retries: u32,
    tool_blacklist: &'static [&'static str],
    instructions: &'static str,
}

impl AgentRuntimeManifest {
    pub fn retries(&self) -> u32 {
        self.retries
    }

    pub fn tool_blacklist(&self) -> &'static [&'static str] {
        self.tool_blacklist
    }

    pub fn instructions(&self) -> &'static str {
        self.instructions
    }
}

/// Static orchestration metadata attached to one baked Agent kind.
#[cfg(feature = "multiagent")]
pub struct MultiagentManifest {
    spawn_enabled: bool,
    allowed_kinds: &'static [AgentKind],
}

#[cfg(feature = "multiagent")]
impl MultiagentManifest {
    pub fn spawn_enabled(&self) -> bool {
        self.spawn_enabled
    }

    pub fn allowed_kinds(&self) -> &'static [AgentKind] {
        self.allowed_kinds
    }
}

pub fn find(kind: &AgentKind) -> Option<&'static AgentCatalogEntry> {
    entries().iter().find(|entry| entry.kind() == kind)
}

pub fn entries() -> &'static [AgentCatalogEntry] {
    ENTRIES
}

/// The sole root kind selected and verified by the manifest generator.
pub fn root_kind() -> &'static AgentKind {
    &ROOT_KIND
}

include!(concat!(env!("OUT_DIR"), "/manifests.rs"));

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn baked_root_kind_is_a_catalog_entry() {
        assert!(find(root_kind()).is_some());
    }

    #[test]
    fn todo_tools_belong_to_the_root_agent() {
        let root = find(root_kind()).expect("root manifest");
        let worker = find(&AgentKind::from_static("worker")).expect("worker manifest");

        assert!(!root.runtime().tool_blacklist().contains(&"todo"));
        assert!(worker.runtime().tool_blacklist().contains(&"todo"));
    }
}

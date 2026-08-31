//! Agent definitions baked into the firmware at compile time.
//!
//! [`AgentRuntimeManifest`] is consumed while assembling one Agent; the
//! optional multiagent manifest is consumed only by the Multiagent extension.

use alloc::{borrow::Cow, string::String};
use core::fmt;
use getset::{CopyGetters, Getters};

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

#[derive(CopyGetters, Getters)]
pub struct AgentCatalogEntry {
    #[getset(get = "pub")]
    kind: AgentKind,
    #[cfg(feature = "multiagent")]
    #[getset(get_copy = "pub")]
    description: &'static str,
    #[getset(get = "pub")]
    runtime: AgentRuntimeManifest,
    #[cfg(feature = "multiagent")]
    #[getset(get = "pub")]
    multiagent: MultiagentManifest,
}

/// Configuration needed to construct one agent in isolation.
#[derive(CopyGetters)]
pub struct AgentRuntimeManifest {
    #[getset(get_copy = "pub")]
    retries: u32,
    #[getset(get_copy = "pub")]
    tool_blacklist: &'static [&'static str],
    #[getset(get_copy = "pub")]
    instructions: &'static str,
}

/// Static orchestration metadata attached to one baked Agent kind.
#[cfg(feature = "multiagent")]
#[derive(CopyGetters)]
pub struct MultiagentManifest {
    #[getset(get_copy = "pub")]
    spawn_enabled: bool,
    #[getset(get_copy = "pub")]
    allowed_kinds: &'static [AgentKind],
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

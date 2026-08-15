#![no_std]
// Tool handles are ref-counted but intentionally executor-local and non-Send.
#![allow(clippy::arc_with_non_send_sync)]

extern crate alloc;
#[cfg(feature = "build-support")]
extern crate std;

#[cfg(feature = "build-support")]
pub mod bake;
mod context;
mod registry;
mod runner;
mod set;
#[allow(clippy::module_inception)]
mod tool;
mod validate;

pub use barracuda_agent_permission::{Action, Resource, RiskClass};
pub use context::{AgentStorage, AgentStorageError, ToolContext};
pub use registry::{ToolGroup, ToolGroupId, ToolRegistry, ToolRegistryError, ToolRegistryVersion};
pub use runner::{ToolDetachHandle, ToolJoinHandle, ToolRunner};
pub use set::{
    ToolCatalogEntry, ToolDiscoveryHandle, ToolGroupCatalog, ToolName, ToolSet, ToolSetError,
    ToolSetHandle,
};
pub use tool::{
    DetachedTool, DetachedToolFuture, DetachedToolHandler, EmptyArgs, Tool, ToolCompletionFuture,
    ToolConfig, ToolError, ToolFuture, ToolHandler, ToolInvocation, ToolInvokeError, ToolOutput,
    ToolResult, ToolSpec,
};

/// Internal bridge used by the Agent runtime to bind framework-owned state.
///
/// Tool implementations should use [`ToolContext::agent_storage`] instead.
#[doc(hidden)]
pub mod runtime {
    pub use crate::context::{AgentStorageBackend, AgentStorageScope};

    use crate::{ToolRunner, ToolSetHandle};

    pub fn agent_runner<'a>(
        tools: &'a ToolSetHandle<'a>,
        storage: AgentStorageScope,
    ) -> ToolRunner<'a> {
        ToolRunner::with_storage(tools, storage)
    }
}

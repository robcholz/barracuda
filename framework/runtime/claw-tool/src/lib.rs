#[cfg(feature = "build-support")]
pub mod bake;
mod detached;
mod executor;
mod registry;
mod set;
#[allow(clippy::module_inception)]
mod tool;
mod validate;

pub use detached::{
    DetachedToolCompletion, DetachedToolInvocation, DetachedToolRun, DetachedToolSink,
};
pub use executor::{ToolExecution, ToolExecutor};
pub use registry::{
    ToolGroup, ToolRegistry, ToolRegistryError, ToolRegistryState, ToolRegistryVersion,
};
pub use set::{
    ToolCatalogEntry, ToolDiscoveryHandle, ToolGroupCatalog, ToolName, ToolSet, ToolSetError,
    ToolSetHandle,
};
pub use tool::{
    AsyncToolHandler, RetryCount, SyncToolHandler, Tool, ToolConfig, ToolError, ToolFuture,
    ToolInvocation, ToolInvokeError, ToolOutput, ToolResult, ToolSpec,
};

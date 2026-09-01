#![no_std]
// Tool handles are ref-counted but intentionally executor-local and non-Send.
#![allow(clippy::arc_with_non_send_sync)]

extern crate alloc;
#[cfg(feature = "build-support")]
extern crate std;

#[cfg(feature = "build-support")]
pub mod bake;
mod definition;
mod registry;
mod runner;
mod set;
mod validate;

pub use barracuda_agent_permission::{Action, Resource, RiskClass};
pub use definition::{
    DetachedTool, DetachedToolFuture, DetachedToolHandler, EmptyArgs, Tool, ToolArgumentsValidator,
    ToolCompletionFuture, ToolConfig, ToolError, ToolFuture, ToolHandler, ToolInvocation,
    ToolInvokeError, ToolOutput, ToolResult, ToolSpec,
};
pub use registry::{ToolGroup, ToolRegistry, ToolRegistryError, ToolRegistryVersion};
pub use runner::{ToolDetachHandle, ToolJoinHandle, ToolRunner};
pub use set::{
    ToolCatalogEntry, ToolDiscoveryHandle, ToolGroupCatalog, ToolName, ToolSet, ToolSetError,
    ToolSetHandle,
};

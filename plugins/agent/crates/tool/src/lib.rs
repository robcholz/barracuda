#![no_std]
// Without atomic compare-and-swap (ESP32-C3, ESP32-S2) `tracing` compiles to
// nothing, so values only traced look unused there.
#![cfg_attr(not(target_has_atomic = "ptr"), allow(unused))]
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
    DetachedTool, DetachedToolFuture, DetachedToolHandler, EmptyArgs, Tool, ToolCompletionFuture,
    ToolConfig, ToolDetachUpdate, ToolError, ToolFuture, ToolHandler, ToolInvocation,
    ToolInvokeError, ToolOutput, ToolProgressSender, ToolResult, ToolSpec,
};
pub use registry::{
    ToolGroup, ToolRegistry, ToolRegistryError, ToolRegistryVersion, ToolSetSource,
};
pub use runner::{ToolDetachHandle, ToolJoinHandle, ToolRunner};
pub use set::{
    ToolCatalogEntry, ToolDiscoveryHandle, ToolGroupCatalog, ToolName, ToolSet, ToolSetError,
    ToolSetHandle,
};

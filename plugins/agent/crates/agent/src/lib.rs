#![cfg_attr(not(test), no_std)]
// Without atomic compare-and-swap (ESP32-C3) `tracing` compiles to
// nothing, so values only traced look unused there.
#![cfg_attr(not(target_has_atomic = "ptr"), allow(unused))]
// Runtime state is executor-local; Arc remains the ownership ABI between crates.
#![allow(clippy::arc_with_non_send_sync)]

//! Single-Agent execution runtime and context-provider implementations.

#[macro_use]
extern crate alloc;

/// Embed a prompt relative to `barracuda-agent/resources/prompt/`.
macro_rules! prompt {
    ($path:literal $(,)?) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/prompt/",
            $path
        ))
    };
}

mod agent_stream;
pub mod baked;
mod config;
mod context_providers;
mod engine;
mod instance;
mod manager;
mod message;
pub(crate) mod tools;

pub use barracuda_runtime_utils::stream;
pub(crate) use barracuda_runtime_utils::{define_id_allocator, define_prefixed_id};

pub use agent_stream::{
    AgentDispatchError, AgentEvent, AgentHandle, AgentStream, AgentStreamItem, AgentTurnOrigin,
};
pub use baked::AgentKind;
pub use barracuda_agent_permission::PermissionLevel;
pub use barracuda_agent_tool::ToolOutput;
pub use barracuda_model_api::ToolCall;
pub use config::{ApiPurpose, ModelApiManager, SharedApiManager};
pub use context_providers::{ReasoningEffort, ReasoningEffortHandle};
pub(crate) use engine::AgentEngineState;
pub use engine::{
    AgentApprovalError, AgentCompletion, AgentError, AgentInputRequest, AgentIterationEvent,
    AgentOutcome, ApprovalDecision, IterationId, IterationLoopError, ToolCallId,
};
pub use instance::Agent;
pub use manager::{
    AgentCreateError, AgentId, AgentIdAllocator, AgentManager, AgentManagerError, PersistenceConfig,
};
pub use message::Message;

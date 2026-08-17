#![cfg_attr(not(test), no_std)]
#![deny(unreachable_pub)]
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
#[doc(hidden)]
pub mod baked;
mod base_agent;
mod config;
mod context_providers;
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
pub(crate) use base_agent::BaseAgentState;
pub use base_agent::{
    AgentApprovalError, AgentCompletion, AgentError as BaseAgentError, AgentInputRequest,
    AgentIterationEvent, AgentOutcome, ApprovalDecision, IterationId, IterationLoopError,
    ToolCallId,
};
pub use config::ApiPurpose;
pub use context_providers::{ReasoningEffort, ReasoningEffortHandle};
pub use instance::Agent;
pub use manager::{
    AgentCreateError, AgentId, AgentIdAllocator, AgentManager, AgentManagerError, PersistenceConfig,
};
pub use message::Message;

/// Workspace-internal contracts consumed by the Session and Multiagent crates.
#[doc(hidden)]
pub mod internal {
    pub use crate::base_agent::AgentError;
    pub use crate::config::SharedApiManager;
    pub use crate::{
        baked, Agent, AgentApprovalError, AgentCompletion, AgentCreateError, AgentDispatchError,
        AgentEvent, AgentHandle, AgentId, AgentIdAllocator, AgentInputRequest, AgentIterationEvent,
        AgentKind, AgentManager, AgentManagerError, AgentOutcome, AgentStream, AgentStreamItem,
        AgentTurnOrigin, ApprovalDecision, IterationId, PersistenceConfig, ReasoningEffort,
        ReasoningEffortHandle, ToolCallId,
    };
}

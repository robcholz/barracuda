#![cfg_attr(not(test), no_std)]
#![deny(unreachable_pub)]
// Runtime state is executor-local; Arc remains the ownership ABI between crates.
#![allow(clippy::arc_with_non_send_sync)]

//! `barracuda_agent_runtime` — execution runtime and agent Session primitives.
//!
//! [`AgentRuntime`] owns process execution; the Session subsystem owns Session
//! lifecycle and actors.

#[macro_use]
extern crate alloc;

/// Embed a prompt relative to `barracuda-agent-runtime/resources/prompt/`.
macro_rules! prompt {
    ($path:literal $(,)?) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/prompt/",
            $path
        ))
    };
}

mod agent;
mod config;
mod message;
#[cfg(feature = "multiagent")]
mod multiagent;
mod runtime;
mod session;

pub use barracuda_runtime_utils::stream;
pub(crate) use barracuda_runtime_utils::{define_id_allocator, define_prefixed_id};

pub use agent::{
    AgentApprovalError, AgentCreateError, AgentError as BaseAgentError, AgentId, IterationId,
    IterationLoopError, ReasoningEffort, ToolCallId,
};
pub use barracuda_agent_permission::PermissionLevel;
pub use barracuda_agent_tool::ToolOutput;
pub use barracuda_model_api::ToolCall;
pub use config::ApiPurpose;
pub use message::Message;
pub use runtime::{AgentRuntime, AgentRuntimeBuildError, AgentService};
pub use session::{
    ApprovalResolverError, ContextProviderError, InputRequestId, InputRequestKind, IterationEvent,
    OpenSessionError, SessionCloseReason, SessionControl, SessionControlError, SessionCreateError,
    SessionDeleteError, SessionError, SessionEvent, SessionEventError, SessionId,
    SessionInputError, SessionPersistence, SessionStream, SessionTurnError, TurnEvent,
    TurnEventError, TurnId, TurnOrigin,
};

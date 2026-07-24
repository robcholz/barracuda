#![deny(unreachable_pub)]

//! `claw_core` — execution runtime and agent Session primitives.
//!
//! [`AgentRuntime`] owns process execution; the Session subsystem owns Session
//! lifecycle and actors.

// The reasoning cap is a crate-wide compile-time tier. Reject missing or
// ambiguous feature selections before any runtime modules are built.
#[cfg(not(any(
    feature = "reasoning_short",
    feature = "reasoning_medium",
    feature = "reasoning_long",
)))]
compile_error!(
    "enable exactly one reasoning tier feature: `reasoning_short`, `reasoning_medium`, or `reasoning_long`"
);
#[cfg(any(
    all(feature = "reasoning_short", feature = "reasoning_medium"),
    all(feature = "reasoning_short", feature = "reasoning_long"),
    all(feature = "reasoning_medium", feature = "reasoning_long"),
))]
compile_error!(
    "enable only one reasoning tier feature: `reasoning_short`, `reasoning_medium`, or `reasoning_long`"
);

/// Embed a prompt relative to `claw-core/resources/prompt/`.
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
#[cfg(feature = "multiagent")]
mod multiagent;
mod runtime;
mod session;

pub(crate) const SYSTEM_TRACE_SCOPE: &str = "agent-system";

pub use claw_utils::stream;
pub(crate) use claw_utils::{define_id_allocator, define_prefixed_id};

pub use agent::{
    AgentApprovalError, AgentCreateError, AgentError as BaseAgentError, AgentId, IterationId,
    IterationLoopError, ReasoningEffort, ToolCallId,
};
pub use claw_api::ToolCall;
pub use claw_permission::PermissionLevel;
pub use claw_tool::ToolOutput;
pub use config::ApiPurpose;
pub use runtime::{AgentRuntime, AgentRuntimeBuildError};
pub use session::{
    ApprovalResolverError, ContextAdapterError, InputRequestId, InputRequestKind, IterationEvent,
    Message, OpenSessionError, SessionCloseReason, SessionControl, SessionControlError,
    SessionCreateError, SessionDeleteError, SessionError, SessionEvent, SessionEventError,
    SessionId, SessionInputError, SessionPersistence, SessionStream, SessionTurnError, TurnEvent,
    TurnEventError, TurnId, TurnOrigin,
};

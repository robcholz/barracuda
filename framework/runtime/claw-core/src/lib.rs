#![deny(unreachable_pub)]

//! `claw_core` — execution runtime and agent Session primitives.
//!
//! [`AgentRuntime`] owns process execution; the Session subsystem owns Session
//! lifecycle and actors.

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
mod runtime;
mod scheduler;
mod session;

pub(crate) const SYSTEM_TRACE_SCOPE: &str = "agent-system";

pub use claw_utils::stream;
pub(crate) use claw_utils::{define_id_allocator, define_prefixed_id};

pub use agent::{AgentId, IterationId};
pub use claw_api::ToolCall;
pub use claw_permission::PermissionLevel;
pub use config::{ApiUsage, ReasoningEffort};
pub use runtime::{AgentRuntime, AgentRuntimeBuildError};
pub use session::{
    InputRequestId, InputRequestKind, Message, OpenSessionError, SessionControl,
    SessionControlError, SessionCreateError, SessionEvent, SessionId, SessionPersistence,
    SessionStream, TurnId, TurnOrigin,
};

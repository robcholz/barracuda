//! One configured Agent and its complete single-Agent runtime.
//!
//! [`BaseAgent::submit`] borrows the Agent exclusively and returns one
//! [`AgentStreamHandle`]. The handle is the only output and control surface for
//! that task; owner-side message queuing remains outside this module.

mod agent;
mod context;
mod effect;
mod iteration_loop;
mod stream;

pub(crate) use self::agent::BaseAgent;
pub(super) use self::agent::BaseAgentConfig;
pub(in crate::agent) use self::context::{ContextAdapter, ContextAdapterFuture, TurnLifecycle};
pub(in crate::agent) use self::effect::{agent_effect_channel, AgentEffect, AgentEffectEmitter};
pub use self::stream::{AgentApprovalError, AgentError};
pub(crate) use self::stream::{
    AgentCompletion, AgentEvent, AgentInputRequest, AgentIterationEvent, AgentOutcome,
    ApprovalDecision,
};
pub use iteration_loop::IterationId;
pub use iteration_loop::{IterationLoopError, ToolCallId};

//! One configured Agent and its complete single-Agent runtime.
//!
//! [`BaseAgent::submit`] executes one linear task for the outer [`super::Agent`].

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
    AgentCompletion, AgentInputRequest, AgentIterationEvent, AgentOutcome, AgentSubmitError,
    ApprovalDecision, BaseAgentEvent,
};
pub use iteration_loop::IterationId;
pub use iteration_loop::{IterationLoopError, ToolCallId};

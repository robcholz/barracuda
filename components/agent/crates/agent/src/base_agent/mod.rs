//! One configured Agent and its complete single-Agent runtime.
//!
//! [`BaseAgent::submit`] executes one linear task for the outer [`super::Agent`].

mod agent;
mod context_provider;
mod effect;
mod iteration_loop;
mod persistence;
mod stream;

pub(crate) use self::agent::BaseAgent;
pub(crate) use self::agent::BaseAgentBuildError;
pub(super) use self::agent::BaseAgentConfig;
pub(crate) use self::context_provider::{
    ContextProvider, ContextProviderFuture, ContextProviderResult, TurnLifecycle,
};
pub(crate) use self::effect::{agent_effect_channel, AgentEffect, AgentEffectEmitter};
pub(crate) use self::persistence::{AgentStorage, BaseAgentState};
pub use self::stream::{AgentApprovalError, AgentError};
pub use self::stream::{
    AgentCompletion, AgentInputRequest, AgentIterationEvent, AgentOutcome, ApprovalDecision,
};
pub(crate) use self::stream::{AgentSubmitError, BaseAgentEvent};
pub use iteration_loop::IterationId;
pub use iteration_loop::{IterationLoopError, ToolCallId};

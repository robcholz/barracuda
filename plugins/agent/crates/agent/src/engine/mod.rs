//! One configured Agent and its complete single-Agent runtime.
//!
//! [`AgentEngine::submit`] executes one linear task for the outer [`super::Agent`].

mod context_provider;
mod driver;
mod effect;
mod iteration_loop;
mod persistence;
mod stream;

pub(crate) use self::context_provider::{
    ContextProvider, ContextProviderFuture, ContextProviderResult, TurnLifecycle,
};
pub(crate) use self::driver::AgentEngine;
pub(crate) use self::driver::AgentEngineBuildError;
pub(super) use self::driver::AgentEngineConfig;
pub(crate) use self::effect::{agent_effect_channel, AgentEffect, AgentEffectEmitter};
pub(crate) use self::persistence::{AgentEngineState, AgentStorage};
pub use self::stream::{AgentApprovalError, AgentError};
pub use self::stream::{
    AgentCompletion, AgentInputRequest, AgentIterationEvent, AgentOutcome, ApprovalDecision,
};
pub(crate) use self::stream::{AgentEngineEvent, AgentSubmitError};
pub use iteration_loop::IterationId;
pub use iteration_loop::{IterationLoopError, ToolCallId};

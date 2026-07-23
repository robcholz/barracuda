//! Runtime for one agent in isolation.
//!
//! This module owns the agent state machine, LLM/tool loop, context adapters,
//! and construction. Graphs, child lifecycles, and subagent tools belong to
//! Multiagent and enter here only as ordinary injected tool groups.

mod agent;
pub(crate) mod baked;
mod base_agent;
mod context_adapters;
mod manager;
mod state;
mod stream;
pub(crate) mod tools;

pub(crate) use agent::Agent;
pub(crate) use baked::AgentKind;
pub use base_agent::{AgentApprovalError, AgentError, IterationId, IterationLoopError, ToolCallId};
pub(crate) use base_agent::{
    AgentCompletion, AgentInputRequest, AgentIterationEvent, AgentOutcome, ApprovalDecision,
};
pub use context_adapters::ReasoningEffort;
pub(crate) use context_adapters::ReasoningEffortHandle;
pub use manager::{AgentCreateError, AgentId};
pub(crate) use manager::{AgentIdAllocator, AgentManager, AgentManagerError, PersistenceConfig};
pub(crate) use stream::{AgentEvent, AgentTurnOrigin};

//! Runtime for one agent in isolation.
//!
//! This module owns the agent state machine, LLM/tool loop, context adapters,
//! and construction. Graphs, child lifecycles, and subagent tools belong to
//! Multiagent and enter here only as ordinary injected tool groups.

pub(crate) mod baked;
mod base_agent;
mod context_adapters;
mod manager;
mod tools;

pub(crate) use baked::AgentKind;
pub use base_agent::IterationId;
pub(crate) use base_agent::{
    AgentApprovalError, AgentCompletion, AgentError, AgentEvent, AgentInputRequest, AgentOutcome,
    AgentState, ApprovalDecision, BaseAgent, IterationEvent, ToolCallId,
};
pub(crate) use context_adapters::ReasoningEffortHandle;
pub use manager::AgentId;
pub(crate) use manager::{
    AdditionalAgentState, AgentCreateError, AgentIdAllocator, AgentManager, AgentManagerError,
    PersistenceConfig,
};

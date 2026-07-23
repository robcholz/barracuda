//! Runtime for one agent in isolation.
//!
//! This module owns the agent state machine, LLM/tool loop, context adapters,
//! and construction. Graphs, child lifecycles, and subagent tools belong to the
//! orchestrator and enter here only as ordinary injected tool groups.

mod base_agent;
mod config;
mod context_adapters;
mod manager;
mod tools;

pub use base_agent::IterationId;
pub(crate) use base_agent::{
    AgentApprovalError, AgentCompletion, AgentError, AgentEvent, AgentInputRequest, AgentOutcome,
    AgentState, ApprovalDecision, BaseAgent, IterationEvent, ToolCallId,
};
pub(crate) use context_adapters::ReasoningEffortHandle;
pub(crate) use manager::{
    AdditionalAgentState, AgentCreateError, AgentManager, AgentManagerError, PersistenceConfig,
};

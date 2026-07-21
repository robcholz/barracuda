//! One configured agent task runner.
//!
//! Commands enter through [`BaseAgent::send_command`]. Each
//! [`BaseAgent::tick`] advances at most one iteration and returns a
//! [`TickOutcome`]; terminal outcomes leave the agent idle and reusable.

mod command;
mod construction;
mod context_adapter;
mod control;
mod iteration;
mod pending_tool_round;
mod reducer;
mod state;
mod task_state;
mod transcript;

use claw_api::{ClawApiAsync, RetryPolicy};
use claw_interface::{ClawHttp, ClawTimer};
use claw_permission::PermissionPolicy;

use super::effect::AgentEffectInbox;
use super::iteration_loop::IterationLoopError;
use crate::protocol::IterationIdAllocator;
use claw_context::{Block, Context};
use claw_tool::ToolSet;
use std::sync::Arc;

pub(crate) use self::command::{
    AgentCommand, AgentCommandError, AgentState, ApprovalDecision, BaseAgentBuildError, TickOutcome,
};
pub(super) use self::construction::BaseAgentConfig;
pub(in crate::agent) use self::context_adapter::{
    ContextAdapter, ContextAdapterFuture, TurnLifecycle,
};
pub(crate) use self::control::AgentAbortHandle;
use self::control::AgentInterruption;
use self::state::BaseAgentState;
pub(in crate::agent) use self::transcript::{AssistantCommit, History, Transcript};

/// A base agent that runs one task at a time as a sequence of iterations.
pub(crate) struct BaseAgent<H: ClawHttp, Timer: ClawTimer> {
    llm: ClawApiAsync<H, Timer>,
    retry_policy: RetryPolicy,
    interruption: AgentInterruption,
    /// Type-erased so context adapters need not carry the filesystem type.
    transcript: Box<dyn Transcript>,
    tools: ToolSet,
    effect_inbox: AgentEffectInbox,
    permission_policy: Arc<dyn PermissionPolicy>,
    agent_instruction: Block<'static>,
    inherited_context: Vec<Block<'static>>,
    /// Derived render/cache state. Authoritative content stays in the fields
    /// above and in `context_adapters`.
    context_cache: Context,
    state: BaseAgentState,
    resume_reminder: Option<String>,
    outcome: Option<TickOutcome>,
    context_adapters: Vec<Box<dyn ContextAdapter>>,
}

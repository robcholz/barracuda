use std::sync::Arc;

use claw_api::{ClawApiAsync, ClawApiConfig, InitError, RetryPolicy};
use claw_context::{Block, BlockKind, Context};
use claw_interface::{ClawHttp, ClawTimer};
use claw_permission::PermissionPolicy;
use claw_tool::ToolSet;

use crate::agent::effect::AgentEffectInbox;
use crate::agent::recovery::{AgentRecoverySnapshot, AgentRecoverySnapshotBuilder};
use crate::agent::AgentResume;

use super::control::AgentInterruption;
use super::state::BaseAgentState;
use super::{BaseAgent, BaseAgentBuildError, ContextAdapter, Transcript};

/// All construction-time configuration for a [`BaseAgent`], consumed by
/// [`BaseAgent::build`].
pub(in crate::agent) struct BaseAgentConfig {
    pub(in crate::agent) transcript: Box<dyn Transcript>,
    pub(in crate::agent) tools: ToolSet,
    pub(in crate::agent) effect_inbox: AgentEffectInbox,
    pub(in crate::agent) permission_policy: Arc<dyn PermissionPolicy>,
    pub(in crate::agent) agent_instruction: Block<'static>,
    pub(in crate::agent) inherited_context: Vec<Block<'static>>,
    pub(in crate::agent) context_adapters: Vec<Box<dyn ContextAdapter>>,
    pub(in crate::agent) retry_policy: RetryPolicy,
    pub(in crate::agent) block_retries: u32,
    pub(in crate::agent) resume: Option<AgentResume>,
}

impl<H: ClawHttp, Timer: ClawTimer> BaseAgent<H, Timer> {
    /// Rebind the agent's LLM client to `config` for the next turn, keeping the
    /// existing transport (see [`ClawApiAsync::set_config`]).
    pub(crate) fn set_llm_config(&mut self, config: ClawApiConfig) -> Result<(), InitError> {
        self.llm.set_config(config)
    }

    pub(crate) fn set_context_block(&mut self, block: Block<'static>) {
        if block.kind == BlockKind::AgentInstruction {
            self.agent_instruction = block.clone();
        } else if let Some(index) = self
            .inherited_context
            .iter()
            .position(|current| current.kind == block.kind)
        {
            if block.content.trim().is_empty() {
                self.inherited_context.remove(index);
            } else {
                self.inherited_context[index] = block.clone();
            }
        } else if !block.content.trim().is_empty() {
            self.inherited_context.push(block.clone());
        }
        self.context_cache.with(block);
    }

    pub(crate) fn loaded_tool_groups(&self) -> Vec<String> {
        self.tools.loaded_groups()
    }

    /// Project durable recovery state without exposing concrete adapter state
    /// to BaseAgent.
    pub(crate) fn recovery_snapshot(&self) -> AgentRecoverySnapshot {
        let mut snapshot = AgentRecoverySnapshotBuilder::new(self.loaded_tool_groups());
        for adapter in &self.context_adapters {
            adapter.contribute_recovery(&mut snapshot);
        }
        snapshot.finish()
    }

    pub(crate) fn resume_pending(&self) -> bool {
        self.resume_reminder.is_some()
    }

    #[cfg(test)]
    pub(crate) fn exposes_tool_for_test(&mut self, name: &str) -> bool {
        let Ok(tools) = self.tools.begin() else {
            return false;
        };
        let Ok(serde_json::Value::Array(schemas)) =
            serde_json::from_str::<serde_json::Value>(tools.schemas_json())
        else {
            return false;
        };
        schemas.iter().any(|schema| {
            schema
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(serde_json::Value::as_str)
                == Some(name)
        })
    }

    /// Assemble a runnable agent from a [`BaseAgentConfig`].
    pub(in crate::agent) fn build(
        config: BaseAgentConfig,
    ) -> Result<BaseAgent<H, Timer>, BaseAgentBuildError>
    where
        H: Default,
        Timer: Default,
    {
        let llm = ClawApiAsync::<H, Timer>::new(H::default(), Timer::default());

        let mut tools = config.tools;
        for adapter in &config.context_adapters {
            if let Some(group) = adapter.tools() {
                tools.add_group(group)?;
            }
        }
        let resume_reminder = config.resume.and_then(render_resume_reminder);

        let mut context_cache = Context::new();
        for block in &config.inherited_context {
            context_cache.with(block.clone());
        }
        context_cache.with(config.agent_instruction.clone());

        Ok(BaseAgent {
            llm,
            retry_policy: config.retry_policy,
            interruption: AgentInterruption::new(),
            transcript: config.transcript,
            tools,
            effect_inbox: config.effect_inbox,
            permission_policy: config.permission_policy,
            agent_instruction: config.agent_instruction,
            inherited_context: config.inherited_context,
            context_cache,
            state: BaseAgentState::new(config.block_retries),
            resume_reminder,
            outcome: None,
            context_adapters: config.context_adapters,
        })
    }
}

fn render_resume_reminder(resume: AgentResume) -> Option<String> {
    let (loaded_groups, inflight_toolcalls) = resume.into_parts();
    let mut details = Vec::new();
    if let Some(detail) = ToolSet::resume_detail(loaded_groups) {
        details.push(detail);
    }
    if !inflight_toolcalls.is_empty() {
        let calls = inflight_toolcalls
            .iter()
            .map(|call| format!("{}({})", call.tool(), call.arguments()))
            .collect::<Vec<_>>()
            .join(", ");
        details.push(format!(
            "tool calls with unknown completion status: {calls}"
        ));
    }
    (!details.is_empty()).then(|| {
        format!(
            "Session resumed after a restart; {}. These runtime-only values were not restored or replayed. Inspect current state before relying on them.",
            details.join("; ")
        )
    })
}

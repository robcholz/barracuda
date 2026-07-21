use std::sync::Arc;

use claw_api::{ClawApiAsync, ClawApiConfig, InitError, RetryPolicy};
use claw_context::{Block, Context};
use claw_interface::{ClawHttp, ClawTimer};
use claw_permission::PermissionPolicy;
use claw_tool::ToolSet;

use crate::agent::effect::AgentEffectInbox;

use super::control::AgentInterruption;
use super::state::BaseAgentState;
use super::{
    AgentState, AgentStateBuilder, BaseAgent, BaseAgentBuildError, ContextAdapter, Transcript,
};

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
}

impl<H: ClawHttp, Timer: ClawTimer> BaseAgent<H, Timer> {
    /// Rebind the agent's LLM client to `config` for the next turn, keeping the
    /// existing transport (see [`ClawApiAsync::set_config`]).
    pub(crate) fn set_llm_config(&mut self, config: ClawApiConfig) -> Result<(), InitError> {
        self.llm.set_config(config)
    }

    /// Project the complete typed Agent DTO without interpreting component
    /// state in BaseAgent.
    pub(crate) fn recovery_state(&self) -> AgentState {
        let mut state = AgentStateBuilder::new();
        for adapter in &self.context_adapters {
            adapter.contribute_state(&mut state);
        }
        state.finish()
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
        let context_adapters = config.context_adapters;
        for adapter in &context_adapters {
            if let Some(group) = adapter.tools() {
                tools.add_group(group)?;
            }
        }
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
            outcome: None,
            context_adapters,
        })
    }
}

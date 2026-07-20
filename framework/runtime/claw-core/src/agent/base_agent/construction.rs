use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use claw_api::{ClawApiAsync, ClawApiConfig, InitError, RetryPolicy};
use claw_context::{Block, Context};
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::TranscriptStore;
use claw_permission::PermissionPolicy;
use claw_skill::SkillSet;
use claw_tool::ToolSet;

use crate::agent::tools::{internal_tools, plan_tools, ControlSink};
use crate::agent::AgentResume;
use crate::memory::{ContextAdapter, SkillContextAdapter, Transcript};

use super::control::AgentInterruption;
use super::mode::AgentMode;
use super::state::BaseAgentState;
use super::{BaseAgent, BaseAgentBuildError};

/// All construction-time configuration for a [`BaseAgent`], consumed by
/// [`BaseAgent::build`].
pub(in crate::agent) struct BaseAgentConfig<F: ClawFs + 'static> {
    pub(in crate::agent) store: TranscriptStore<F>,
    pub(in crate::agent) tools: ToolSet,
    pub(in crate::agent) permission_policy: Arc<dyn PermissionPolicy>,
    pub(in crate::agent) skills: SkillSet,
    pub(in crate::agent) agent_instruction: Block<'static>,
    pub(in crate::agent) inherited_context: Vec<Block<'static>>,
    pub(in crate::agent) retry_policy: RetryPolicy,
    pub(in crate::agent) block_retries: u32,
    pub(in crate::agent) initial_mode: AgentMode,
    pub(in crate::agent) resume: Option<AgentResume>,
}

impl<H: ClawHttp, Timer: ClawTimer> BaseAgent<H, Timer> {
    /// Rebind the agent's LLM client to `config` for the next turn, keeping the
    /// existing transport (see [`ClawApiAsync::set_config`]).
    pub(crate) fn set_llm_config(&mut self, config: ClawApiConfig) -> Result<(), InitError> {
        self.llm.set_config(config)
    }

    pub(crate) fn set_context_block(&mut self, block: Block<'static>) {
        self.context.with(block);
    }

    pub(crate) fn mode(&self) -> AgentMode {
        self.state.mode
    }

    pub(crate) fn loaded_tool_groups(&self) -> Vec<String> {
        self.tools.loaded_groups()
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
    pub(in crate::agent) fn build<F: ClawFs + 'static>(
        config: BaseAgentConfig<F>,
    ) -> Result<BaseAgent<H, Timer>, BaseAgentBuildError>
    where
        H: Default,
        Timer: Default,
    {
        let control: ControlSink = Arc::new(Mutex::new(VecDeque::new()));

        let llm = ClawApiAsync::<H, Timer>::new(H::default(), Timer::default());

        let mut tools = config.tools;
        tools.add_group(internal_tools(Arc::clone(&control)))?;
        tools.add_group(plan_tools(Arc::clone(&control)))?;
        let resume_reminder = config.resume.and_then(render_resume_reminder);

        let mut context = Context::new();
        for block in config.inherited_context {
            context.with(block);
        }
        context.with(config.agent_instruction);

        let skill_adapter: Box<dyn ContextAdapter> =
            Box::new(SkillContextAdapter::new(config.skills));
        let transcript: Box<dyn Transcript> = Box::new(config.store);
        if let Some(group) = skill_adapter.tools() {
            tools.add_group(group)?;
        }
        let adapters = vec![skill_adapter];

        Ok(BaseAgent {
            llm,
            retry_policy: config.retry_policy,
            interruption: AgentInterruption::new(),
            transcript,
            tools,
            permission_policy: config.permission_policy,
            context,
            state: BaseAgentState::new(config.block_retries, config.initial_mode),
            resume_reminder,
            outcome: None,
            control,
            adapters,
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

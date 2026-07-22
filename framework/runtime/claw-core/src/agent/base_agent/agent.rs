use std::sync::Arc;

use claw_api::{ClawApiAsync, RetryPolicy};
use claw_context::{Block, BlockKind, Context};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};
use claw_permission::{PermissionDecision, PermissionPolicy, PermissionRequest};
use claw_tool::ToolSet;
use tracing::Instrument as _;

use crate::config::{ApiUsage, SharedApiManager};
use crate::protocol::{IterationId, IterationIdAllocator, Message};

use super::context::{AssistantCommit, ContextAdapter, Transcript};
use super::effect::{AgentEffect, AgentEffectInbox};
use super::iteration_loop::{
    AppendedMessages, IterationLoop, IterationLoopError, IterationOutcome, LlmStep,
    ToolAuthorization, ToolPermission, ToolPermissionPolicy, ToolPermissionRequest,
};
use super::persistence::{AgentState as RecoveryState, AgentStateBuilder};
use super::stream::{
    AgentError, AgentProgress, AgentStreamHandle, AgentSubmitError, ApprovalOutcome,
    ProgressEmitter, RunControl,
};
use super::TurnLifecycle;

/// All construction-time dependencies for one fully assembled BaseAgent.
pub(in crate::agent) struct BaseAgentConfig {
    pub(in crate::agent) transcript: Box<dyn Transcript>,
    pub(in crate::agent) agent_instruction: Block<'static>,
    pub(in crate::agent) inherited_context: Vec<Block<'static>>,
    pub(in crate::agent) context_adapters: Vec<Box<dyn ContextAdapter>>,
    pub(in crate::agent) api_manager: SharedApiManager,
    pub(in crate::agent) api_usage: ApiUsage,
    pub(in crate::agent) tools: ToolSet,
    pub(in crate::agent) effect_inbox: AgentEffectInbox,
    pub(in crate::agent) permission_policy: Arc<dyn PermissionPolicy>,
    pub(in crate::agent) retry_policy: RetryPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopReason {
    Ready,
    Completed,
    Interrupted,
    Cancelled,
    Failed,
}

enum AgentState {
    Running,
    Stopped(StopReason),
}

/// One configured Agent and its complete single-Agent state machine.
pub(crate) struct BaseAgent<H: ClawHttp, Timer: ClawTimer> {
    llm: ClawApiAsync<H, Timer>,
    api_manager: SharedApiManager,
    api_usage: ApiUsage,
    retry_policy: RetryPolicy,
    transcript: Box<dyn Transcript>,
    tools: ToolSet,
    effect_inbox: AgentEffectInbox,
    permission_policy: Arc<dyn PermissionPolicy>,
    context: Context,
    state: AgentState,
    iteration_ids: IterationIdAllocator,
    context_adapters: Vec<Box<dyn ContextAdapter>>,
}

impl<H: ClawHttp + StreamingHttp, Timer: ClawTimer> BaseAgent<H, Timer> {
    pub(in crate::agent) fn build(config: BaseAgentConfig) -> Result<Self, claw_tool::ToolSetError>
    where
        H: Default,
        Timer: Default,
    {
        let mut tools = config.tools;
        for adapter in &config.context_adapters {
            if let Some(group) = adapter.tools() {
                tools.add_group(group)?;
            }
        }

        let mut context = Context::new();
        for block in &config.inherited_context {
            context.with(block.clone());
        }
        context.with(config.agent_instruction);

        Ok(Self {
            llm: ClawApiAsync::new(H::default(), Timer::default()),
            api_manager: config.api_manager,
            api_usage: config.api_usage,
            retry_policy: config.retry_policy,
            transcript: config.transcript,
            tools,
            effect_inbox: config.effect_inbox,
            permission_policy: config.permission_policy,
            context,
            state: AgentState::Stopped(StopReason::Ready),
            iteration_ids: IterationIdAllocator::new(),
            context_adapters: config.context_adapters,
        })
    }

    /// Submit one message and borrow this Agent exclusively until its progress
    /// stream is dropped or reaches a terminal item.
    pub(crate) fn submit(
        &mut self,
        message: Message,
    ) -> Result<AgentStreamHandle<'_>, AgentSubmitError> {
        self.begin(message)?;

        let control = AgentStreamHandle::control();
        let (progress, receiver) = AgentStreamHandle::channel();
        let active = ActiveRun::new(self);
        let driver = Box::pin(active.drive(progress, control.clone()));
        Ok(AgentStreamHandle::new(driver, receiver, control))
    }

    pub(crate) fn recovery_state(&self) -> RecoveryState {
        let mut state = AgentStateBuilder::new();
        for adapter in &self.context_adapters {
            adapter.contribute_state(&mut state);
        }
        state.finish()
    }

    fn begin(&mut self, message: Message) -> Result<(), AgentSubmitError> {
        let AgentState::Stopped(previous) = self.state else {
            return Err(AgentSubmitError::Running);
        };
        tracing::debug!(name: "agent_message_accepted", previous = ?previous);
        self.iteration_ids = IterationIdAllocator::new();
        self.transcript.append_user(message.as_str(), true);
        self.state = AgentState::Running;
        Ok(())
    }

    async fn drive(&mut self, progress: &ProgressEmitter, control: &RunControl) {
        loop {
            let next = match &self.state {
                AgentState::Stopped(_) => Some(self.fail(AgentError::StateInvariant)),
                AgentState::Running => {
                    if control.take_interrupt() {
                        self.stop(StopReason::Interrupted);
                        Some(AgentProgress::Interrupted)
                    } else {
                        let iteration = self.iteration_ids.next();
                        let result = self.run_iteration(iteration, progress, control).await;
                        self.reduce_iteration(result, control)
                    }
                }
            };

            if let Some(next) = next {
                let terminal = next.is_terminal();
                progress.send(next).await;
                if terminal {
                    return;
                }
            }
        }
    }

    async fn run_iteration(
        &mut self,
        iteration_id: IterationId,
        progress: &ProgressEmitter,
        control: &RunControl,
    ) -> Result<IterationOutcome, IterationLoopError> {
        self.refresh_llm_config();

        let adapter_count = self.context_adapters.len() as u64;
        let prepare_span = tracing::info_span!(
            "iteration.prepare",
            run.iteration = %iteration_id,
            adapter_count,
        );
        self.prepare_adapter_context()
            .instrument(prepare_span.clone())
            .await;

        let render_span =
            prepare_span.in_scope(|| tracing::info_span!("context.render", adapter_count));
        let history = render_span.in_scope(|| self.render_adapter_context());
        let tools = render_span.in_scope(|| self.tools.begin())?;
        render_span.in_scope(|| {
            self.context
                .with(Block::new(BlockKind::ToolPolicy, tools.tool_context()))
                .with_reminder(BlockKind::ToolReminder, Some(tools.extra_tool_context()));
        });

        let context = render_span.in_scope(|| self.context.request(&history));
        let step = LlmStep {
            iteration_id,
            system_prompt: context.system(),
            messages: context.history(),
            reminders: context.reminders(),
            tools: &tools,
        };
        drop(render_span);
        drop(prepare_span);

        let permission = BaseAgentPermissionPolicy {
            policy: self.permission_policy.as_ref(),
            progress,
            control,
        };
        IterationLoop {
            llm: &mut self.llm,
            control,
            permission: &permission,
            retry: self.retry_policy,
            progress,
        }
        .run(step)
        .await
    }

    fn reduce_iteration(
        &mut self,
        result: Result<IterationOutcome, IterationLoopError>,
        control: &RunControl,
    ) -> Option<AgentProgress> {
        match result {
            Ok(IterationOutcome::Response(response)) => {
                let Some(text) = response.text else {
                    return Some(self.fail(AgentError::from(
                        IterationLoopError::MalformedAssistantMessage,
                    )));
                };
                let commit = match response.raw_message_json.as_deref() {
                    Some(raw) => AssistantCommit::RawJson(raw),
                    None => AssistantCommit::PlainText(&text),
                };
                self.transcript.commit_assistant(commit);
                self.stop(StopReason::Completed);
                Some(AgentProgress::Yielded { text })
            }
            Ok(IterationOutcome::Tools(appended)) => {
                self.transcript.append_patch(&appended.into_json_array());
                if let Some(progress) = self.reduce_agent_effects() {
                    return Some(progress);
                }
                if control.take_interrupt() {
                    self.stop(StopReason::Interrupted);
                    return Some(AgentProgress::Interrupted);
                }
                None
            }
            Ok(IterationOutcome::Interrupted) => {
                self.abandon_open_task();
                self.stop(StopReason::Interrupted);
                Some(AgentProgress::Interrupted)
            }
            Ok(IterationOutcome::Cancelled(produced)) => {
                self.merge_cancelled_patch(produced);
                self.stop(StopReason::Cancelled);
                Some(AgentProgress::Cancelled)
            }
            Err(error) => Some(self.fail(error.into())),
        }
    }

    fn reduce_agent_effects(&mut self) -> Option<AgentProgress> {
        let mut effects = self.effect_inbox.drain();
        if effects.len() > 1 {
            let count = effects.len();
            tracing::error!(name: "agent_effect_conflict", count = count as u64);
            return Some(self.fail(AgentError::ConflictingEffects { count }));
        }
        effects.pop().map(|effect| self.reduce_tool_effect(effect))
    }

    fn reduce_tool_effect(&mut self, effect: AgentEffect) -> AgentProgress {
        match effect {
            AgentEffect::Finish { final_message } => {
                self.transcript.commit_ended(&final_message);
                self.stop(StopReason::Completed);
                AgentProgress::Ended { final_message }
            }
            AgentEffect::Yield { message } => {
                self.transcript
                    .commit_assistant(AssistantCommit::PlainText(&message));
                self.stop(StopReason::Completed);
                AgentProgress::YieldedByTool { text: message }
            }
        }
    }

    fn fail(&mut self, error: AgentError) -> AgentProgress {
        self.effect_inbox.clear();
        self.stop(StopReason::Failed);
        AgentProgress::Failed(error)
    }

    fn merge_cancelled_patch(&mut self, produced: AppendedMessages) {
        if produced.is_empty() {
            return;
        }
        let Some(patch) = produced.into_complete_json_array() else {
            tracing::warn!(name = "cancelled_patch_dropped");
            return;
        };
        self.transcript.append_patch(&patch);
    }

    async fn prepare_adapter_context(&mut self) {
        let history = self.transcript.as_history();
        for adapter in &mut self.context_adapters {
            adapter.prepare(history).await;
        }
    }

    fn render_adapter_context(&mut self) -> serde_json::Value {
        let mut sink = self.context.sink();
        for adapter in &mut self.context_adapters {
            adapter.contribute(&mut sink);
        }
        sink.into_history()
    }

    fn refresh_llm_config(&mut self) {
        let config = self
            .api_manager
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_api(self.api_usage);
        if let Some(config) = config {
            if self.llm.set_config(config).is_err() {
                tracing::error!(name: "llm_config_invalid", usage = ?self.api_usage);
            }
        }
    }
}

impl<H: ClawHttp, Timer: ClawTimer> BaseAgent<H, Timer> {
    pub(crate) fn is_stopped(&self) -> bool {
        matches!(self.state, AgentState::Stopped(_))
    }

    fn stop(&mut self, reason: StopReason) {
        self.state = AgentState::Stopped(reason);
        for adapter in &mut self.context_adapters {
            adapter.on_turn_lifecycle(TurnLifecycle::Ended);
        }
    }

    fn abandon_open_task(&mut self) {
        self.effect_inbox.clear();
        self.transcript.discard_open_turn();
    }
}

struct BaseAgentPermissionPolicy<'a> {
    policy: &'a dyn PermissionPolicy,
    progress: &'a ProgressEmitter,
    control: &'a RunControl,
}

impl ToolPermissionPolicy for BaseAgentPermissionPolicy<'_> {
    fn authorize<'a>(&'a self, request: ToolPermissionRequest<'_>) -> ToolAuthorization<'a> {
        match self
            .policy
            .evaluate(&PermissionRequest::new(request.action))
        {
            PermissionDecision::Allow => ToolAuthorization::Allow,
            PermissionDecision::Deny { reason } => ToolAuthorization::Deny(reason),
            PermissionDecision::Ask { reason } => {
                let tool_call_id = request.tool_call_id;
                ToolAuthorization::Pending(Box::pin(async move {
                    self.control.begin_approval(tool_call_id);
                    self.progress
                        .send(AgentProgress::ApprovalRequired {
                            tool_call_id,
                            summary: reason,
                        })
                        .await;
                    match self.control.approval().await {
                        ApprovalOutcome::Decision(decision) => match decision {
                            super::stream::ApprovalDecision::Approved => ToolPermission::Allow,
                            super::stream::ApprovalDecision::Rejected(reason) => {
                                ToolPermission::Deny(reason)
                            }
                        },
                        ApprovalOutcome::Interrupted => ToolPermission::Interrupted,
                        ApprovalOutcome::Cancelled => ToolPermission::Cancelled,
                    }
                }))
            }
        }
    }
}

struct ActiveRun<'a, H: ClawHttp, Timer: ClawTimer> {
    agent: &'a mut BaseAgent<H, Timer>,
    finished: bool,
}

impl<'a, H, Timer> ActiveRun<'a, H, Timer>
where
    H: ClawHttp + StreamingHttp,
    Timer: ClawTimer,
{
    fn new(agent: &'a mut BaseAgent<H, Timer>) -> Self {
        Self {
            agent,
            finished: false,
        }
    }

    async fn drive(mut self, progress: ProgressEmitter, control: RunControl) {
        self.agent.drive(&progress, &control).await;
        self.finished = true;
    }
}

impl<H: ClawHttp, Timer: ClawTimer> Drop for ActiveRun<'_, H, Timer> {
    fn drop(&mut self) {
        if self.finished || self.agent.is_stopped() {
            return;
        }
        self.agent.abandon_open_task();
        self.agent.stop(StopReason::Cancelled);
    }
}

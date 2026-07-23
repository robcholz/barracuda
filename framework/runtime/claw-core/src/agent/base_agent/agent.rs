use std::sync::Arc;

use claw_api::{ChatStreamEvent, ClawApiAsync, RetryPolicy, ToolCall};
use claw_context::{Block, BlockKind, Context};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};
use claw_memory::{AssistantFinish, Transcript, TurnHandle};
use claw_permission::{PermissionDecision, PermissionPolicy, PermissionRequest};
use claw_persistence::DurableState;
use claw_tool::ToolSet;
use claw_utils::stream::StreamPart;
use futures_lite::StreamExt as _;
use getset::Getters;
use tracing::Instrument as _;

use crate::agent::AgentKind;
use crate::config::{ApiUsage, SharedApiManager};
use crate::session::Message;

use super::context::ContextAdapter;
use super::effect::{AgentEffect, AgentEffectInbox};
use super::iteration_loop::{
    IterationEvent, IterationIdAllocator, IterationLoop, IterationLoopError, IterationLoopEvent,
    LlmStep, ToolAuthorization, ToolPermission, ToolPermissionPolicy, ToolPermissionRequest,
};
use super::persistence::{AgentState as RecoveryState, AgentStateBuilder};
use super::stream::{
    AgentCompletion, AgentError, AgentEvent, AgentInputRequest, AgentOutcome, AgentStreamHandle,
    AgentSubmitError, ApprovalOutcome, RunControl,
};
use super::TurnLifecycle;

/// All construction-time dependencies for one fully assembled BaseAgent.
pub(in crate::agent) struct BaseAgentConfig {
    pub(in crate::agent) kind: AgentKind,
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

enum IterationCompletion {
    Response(String),
    Tools,
    Interrupted,
    Cancelled,
}

/// One configured Agent and its complete single-Agent state machine.
#[derive(Getters)]
pub(crate) struct BaseAgent<H: ClawHttp, Timer: ClawTimer> {
    kind: AgentKind,
    recovery_state: DurableState<RecoveryState>,
    llm: ClawApiAsync<H, Timer>,
    api_manager: SharedApiManager,
    api_usage: ApiUsage,
    retry_policy: RetryPolicy,
    transcript: Box<dyn Transcript>,
    active_turn: Option<TurnHandle>,
    tools: ToolSet,
    effect_inbox: AgentEffectInbox,
    permission_policy: Arc<dyn PermissionPolicy>,
    #[getset(get = "pub(crate)")]
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

        let recovery_state = DurableState::new(Self::collect_recovery_state(
            config.kind.clone(),
            &config.context_adapters,
        ));

        Ok(Self {
            kind: config.kind,
            recovery_state,
            llm: ClawApiAsync::new(H::default(), Timer::default()),
            api_manager: config.api_manager,
            api_usage: config.api_usage,
            retry_policy: config.retry_policy,
            transcript: config.transcript,
            active_turn: None,
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
        let stream = self.run_stream(control.clone());
        Ok(AgentStreamHandle::new(stream, control))
    }

    fn run_stream(
        &mut self,
        control: RunControl,
    ) -> impl futures_core::Stream<Item = Result<AgentEvent, AgentError>> + '_ {
        ActiveRunGuard::new(self).into_stream(control)
    }

    fn begin(&mut self, message: Message) -> Result<(), AgentSubmitError> {
        let AgentState::Stopped(previous) = self.state else {
            return Err(AgentSubmitError::Running);
        };
        tracing::debug!(name: "agent_message_accepted", previous = ?previous);
        self.iteration_ids = IterationIdAllocator::new();
        let mut turn = self.transcript.open_turn()?;
        turn.append_user(message.as_str())?;
        turn.finish_user()?;
        self.active_turn = Some(turn);
        self.state = AgentState::Running;
        Ok(())
    }

    fn reduce_iteration(
        &mut self,
        result: Result<IterationCompletion, AgentError>,
        control: &RunControl,
    ) -> Result<Option<AgentEvent>, AgentError> {
        Ok(match result? {
            IterationCompletion::Response(text) => {
                self.commit_active_turn()?;
                self.stop(StopReason::Completed);
                Some(AgentEvent::Finished(AgentOutcome::Completed(
                    AgentCompletion::Streamed(text),
                )))
            }
            IterationCompletion::Tools => {
                self.refresh_recovery_state();
                if let Some(event) = self.reduce_agent_effects()? {
                    return Ok(Some(event));
                }
                if control.take_interrupt() {
                    self.abandon_open_task();
                    self.stop(StopReason::Interrupted);
                    return Ok(Some(AgentEvent::Finished(AgentOutcome::Interrupted)));
                }
                None
            }
            IterationCompletion::Interrupted => {
                self.abandon_open_task();
                self.stop(StopReason::Interrupted);
                Some(AgentEvent::Finished(AgentOutcome::Interrupted))
            }
            IterationCompletion::Cancelled => {
                self.abandon_open_task();
                self.stop(StopReason::Cancelled);
                Some(AgentEvent::Finished(AgentOutcome::Cancelled))
            }
        })
    }

    fn reduce_agent_effects(&mut self) -> Result<Option<AgentEvent>, AgentError> {
        let mut effects = self.effect_inbox.drain();
        if effects.len() > 1 {
            let count = effects.len();
            tracing::error!(name: "agent_effect_conflict", count = count as u64);
            return Err(AgentError::ConflictingEffects { count });
        }
        effects
            .pop()
            .map(|effect| self.reduce_tool_effect(effect))
            .transpose()
    }

    fn reduce_tool_effect(&mut self, effect: AgentEffect) -> Result<AgentEvent, AgentError> {
        let message = match effect {
            AgentEffect::Finish { final_message } => {
                self.finish_synthesized_assistant(&final_message)?;
                self.commit_active_turn()?;
                self.stop(StopReason::Completed);
                final_message
            }
            AgentEffect::Yield { message } => {
                self.finish_synthesized_assistant(&message)?;
                self.commit_active_turn()?;
                self.stop(StopReason::Completed);
                message
            }
        };
        Ok(AgentEvent::Finished(AgentOutcome::Completed(
            AgentCompletion::Synthesized(message),
        )))
    }

    fn fail(&mut self, error: AgentError) -> AgentError {
        self.abandon_open_task();
        self.stop(StopReason::Failed);
        error
    }

    fn finish_synthesized_assistant(&mut self, message: &str) -> Result<(), AgentError> {
        let turn = self
            .active_turn
            .as_mut()
            .ok_or(AgentError::StateInvariant)?;
        turn.finish_assistant(AssistantFinish::PlainText(message))?;
        Ok(())
    }

    fn commit_active_turn(&mut self) -> Result<(), AgentError> {
        let turn = self.active_turn.take().ok_or(AgentError::StateInvariant)?;
        turn.commit()?;
        Ok(())
    }

    async fn prepare_adapter_context(&mut self) {
        for adapter in &mut self.context_adapters {
            adapter.prepare(self.transcript.as_ref()).await;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentPhase {
    Reasoning,
    Output,
    ToolCalls,
    Ended,
}

#[derive(Default)]
struct AssistantDraft {
    reasoning: String,
    response: Option<String>,
    tool_calls: Vec<ToolCall>,
}

impl AssistantDraft {
    fn take_tool_calls_json(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.tool_calls)
            .into_iter()
            .map(|call| {
                serde_json::json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": call.arguments_json,
                    },
                })
            })
            .collect()
    }
}

/// BaseAgent's consuming half of an iteration.
///
/// It is deliberately the only place that knows both transcript semantics and
/// owner-facing progress semantics: each chat stream event is incorporated into the
/// active turn before the corresponding progress item is forwarded.
struct IterationConsumer<'a> {
    turn: &'a mut TurnHandle,
    draft: AssistantDraft,
    phase: ContentPhase,
    reasoning_bytes: usize,
    assistant_finished: bool,
    saw_tool_calls: bool,
}

impl<'a> IterationConsumer<'a> {
    fn new(turn: &'a mut TurnHandle) -> Self {
        Self {
            turn,
            draft: AssistantDraft::default(),
            phase: ContentPhase::Reasoning,
            reasoning_bytes: 0,
            assistant_finished: false,
            saw_tool_calls: false,
        }
    }

    /// Apply one backend event to the transcript before returning the matching
    /// owner-facing item. `None` means the event was intentionally suppressed.
    fn consume_chat_event(
        &mut self,
        event: ChatStreamEvent,
    ) -> Result<Option<ChatStreamEvent>, AgentError> {
        debug_assert!(!self.assistant_finished);
        let progress = match event {
            ChatStreamEvent::Reasoning(StreamPart::Delta(fragment)) => {
                debug_assert_eq!(self.phase, ContentPhase::Reasoning);
                self.draft.reasoning.push_str(&fragment);
                self.reasoning_progress(fragment)
            }
            ChatStreamEvent::Reasoning(StreamPart::End) => {
                debug_assert_eq!(self.phase, ContentPhase::Reasoning);
                self.phase = ContentPhase::Output;
                Some(ChatStreamEvent::Reasoning(StreamPart::End))
            }
            ChatStreamEvent::Output(StreamPart::Delta(fragment)) => {
                debug_assert_eq!(self.phase, ContentPhase::Output);
                // Mutate the transcript first. If that fails, the fragment must
                // not appear on the owner-facing stream as if it were durable.
                self.turn.append_assistant(&fragment)?;
                Some(ChatStreamEvent::Output(StreamPart::Delta(fragment)))
            }
            ChatStreamEvent::Output(StreamPart::End) => {
                debug_assert_eq!(self.phase, ContentPhase::Output);
                self.phase = ContentPhase::ToolCalls;
                Some(ChatStreamEvent::Output(StreamPart::End))
            }
            ChatStreamEvent::ToolCalls(StreamPart::Delta(call)) => {
                debug_assert_eq!(self.phase, ContentPhase::ToolCalls);
                self.draft.tool_calls.push(call.clone());
                Some(ChatStreamEvent::ToolCalls(StreamPart::Delta(call)))
            }
            ChatStreamEvent::ToolCalls(StreamPart::End) => {
                debug_assert_eq!(self.phase, ContentPhase::ToolCalls);
                // At this boundary the complete backend-shaped assistant
                // message is known. Finish it before exposing the End marker.
                self.finish_assistant()?;
                self.phase = ContentPhase::Ended;
                Some(ChatStreamEvent::ToolCalls(StreamPart::End))
            }
        };
        Ok(progress)
    }

    fn reasoning_progress(&mut self, mut fragment: String) -> Option<ChatStreamEvent> {
        debug_assert_eq!(self.phase, ContentPhase::Reasoning);
        if fragment.is_empty() || self.reasoning_bytes >= reasoning_limit() {
            return None;
        }
        let remaining = reasoning_limit() - self.reasoning_bytes;
        let mut end = remaining.min(fragment.len());
        while end > 0 && !fragment.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            return None;
        }
        self.reasoning_bytes += end;
        fragment.truncate(end);
        Some(ChatStreamEvent::Reasoning(StreamPart::Delta(fragment)))
    }

    fn finish_content(&mut self) -> Vec<ChatStreamEvent> {
        let mut progress = Vec::with_capacity(3);
        if self.phase == ContentPhase::Reasoning {
            progress.push(ChatStreamEvent::Reasoning(StreamPart::End));
            self.phase = ContentPhase::Output;
        }
        if self.phase == ContentPhase::Output {
            progress.push(ChatStreamEvent::Output(StreamPart::End));
            self.phase = ContentPhase::ToolCalls;
        }
        if self.phase == ContentPhase::ToolCalls {
            progress.push(ChatStreamEvent::ToolCalls(StreamPart::End));
            self.phase = ContentPhase::Ended;
        }
        progress
    }

    fn finish_iteration(&mut self) -> Result<IterationCompletion, AgentError> {
        self.finish_assistant()?;
        if self.saw_tool_calls {
            Ok(IterationCompletion::Tools)
        } else {
            self.draft
                .response
                .take()
                .filter(|message| !message.is_empty())
                .map(IterationCompletion::Response)
                .ok_or(AgentError::MalformedAssistantMessage)
        }
    }

    fn finish_assistant(&mut self) -> Result<(), AgentError> {
        if self.assistant_finished {
            return Ok(());
        }
        let tool_calls = self.draft.take_tool_calls_json();
        self.saw_tool_calls = !tool_calls.is_empty();
        self.draft.response = self
            .turn
            .finish_streamed_assistant(std::mem::take(&mut self.draft.reasoning), tool_calls)?;
        self.assistant_finished = true;
        Ok(())
    }
}

const fn reasoning_limit() -> usize {
    #[cfg(feature = "reasoning_short")]
    {
        2_000
    }
    #[cfg(all(feature = "reasoning_medium", not(feature = "reasoning_short")))]
    {
        8_000
    }
    #[cfg(all(
        feature = "reasoning_long",
        not(feature = "reasoning_short"),
        not(feature = "reasoning_medium")
    ))]
    {
        32_000
    }
}

impl<H: ClawHttp, Timer: ClawTimer> BaseAgent<H, Timer> {
    pub(crate) fn recovery_state(&self) -> &DurableState<RecoveryState> {
        self.refresh_recovery_state();
        &self.recovery_state
    }

    fn collect_recovery_state(
        kind: AgentKind,
        adapters: &[Box<dyn ContextAdapter>],
    ) -> RecoveryState {
        let mut state = AgentStateBuilder::new(kind);
        for adapter in adapters {
            adapter.contribute_state(&mut state);
        }
        state.finish()
    }

    fn refresh_recovery_state(&self) {
        let next = Self::collect_recovery_state(self.kind.clone(), &self.context_adapters);
        let changed = self.recovery_state.get().ne(&next);
        if changed {
            self.recovery_state.replace(next);
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        matches!(self.state, AgentState::Stopped(_))
    }

    fn stop(&mut self, reason: StopReason) {
        self.state = AgentState::Stopped(reason);
        for adapter in &mut self.context_adapters {
            adapter.on_turn_lifecycle(TurnLifecycle::Ended);
        }
        self.refresh_recovery_state();
    }

    fn abandon_open_task(&mut self) {
        self.effect_inbox.clear();
        if let Some(turn) = self.active_turn.take() {
            turn.discard();
        }
    }
}

struct BaseAgentPermissionPolicy<'a> {
    policy: &'a dyn PermissionPolicy,
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
                let activate_control = self.control;
                let decision_control = self.control;
                ToolAuthorization::Pending {
                    reason,
                    activate: Box::new(move || activate_control.begin_approval(tool_call_id)),
                    permission: Box::pin(async move {
                        match decision_control.approval().await {
                            ApprovalOutcome::Decision(decision) => match decision {
                                super::stream::ApprovalDecision::Approved => ToolPermission::Allow,
                                super::stream::ApprovalDecision::Rejected(reason) => {
                                    ToolPermission::Deny(reason)
                                }
                            },
                            ApprovalOutcome::Interrupted => ToolPermission::Interrupted,
                            ApprovalOutcome::Cancelled => ToolPermission::Cancelled,
                        }
                    }),
                }
            }
        }
    }
}

/// Restores BaseAgent's stopped-state invariant if its borrowing stream is
/// dropped before producing a terminal event or error.
struct ActiveRunGuard<'a, H: ClawHttp, Timer: ClawTimer> {
    agent: &'a mut BaseAgent<H, Timer>,
}

impl<'a, H, Timer> ActiveRunGuard<'a, H, Timer>
where
    H: ClawHttp + StreamingHttp,
    Timer: ClawTimer,
{
    fn new(agent: &'a mut BaseAgent<H, Timer>) -> Self {
        Self { agent }
    }

    fn into_stream(
        self,
        control: RunControl,
    ) -> impl futures_core::Stream<Item = Result<AgentEvent, AgentError>> + 'a {
        async_stream::stream! {
            loop {
                if !matches!(self.agent.state, AgentState::Running) {
                    yield Err(self.agent.fail(AgentError::StateInvariant));
                    break;
                }
                if control.take_interrupt() {
                    self.agent.abandon_open_task();
                    self.agent.stop(StopReason::Interrupted);
                    yield Ok(AgentEvent::Finished(AgentOutcome::Interrupted));
                    break;
                }

                let iteration_id = self.agent.iteration_ids.next();
                self.agent.refresh_llm_config();
                if self.agent.active_turn.is_none() {
                    yield Err(self.agent.fail(AgentError::StateInvariant));
                    break;
                }

                let adapter_count = self.agent.context_adapters.len() as u64;
                let prepare_span = tracing::info_span!(
                    "iteration.prepare",
                    run.iteration = %iteration_id,
                    adapter_count,
                );
                self.agent
                    .prepare_adapter_context()
                    .instrument(prepare_span.clone())
                    .await;

                let render_span = prepare_span
                    .in_scope(|| tracing::info_span!("context.render", adapter_count));
                let history = render_span.in_scope(|| self.agent.render_adapter_context());
                self.agent.refresh_recovery_state();
                let tools = match render_span.in_scope(|| self.agent.tools.begin()) {
                    Ok(tools) => tools,
                    Err(error) => {
                        yield Err(self.agent.fail(AgentError::from(
                            IterationLoopError::from(error),
                        )));
                        break;
                    }
                };
                render_span.in_scope(|| {
                    self.agent
                        .context
                        .with(Block::new(BlockKind::ToolPolicy, tools.tool_context()))
                        .with_reminder(
                            BlockKind::ToolReminder,
                            Some(tools.extra_tool_context()),
                        );
                });

                let context = render_span.in_scope(|| self.agent.context.request(&history));
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
                    policy: self.agent.permission_policy.as_ref(),
                    control: &control,
                };
                let mut iteration = Box::pin(IterationLoop {
                    llm: &mut self.agent.llm,
                    control: &control,
                    permission: &permission,
                    retry: self.agent.retry_policy,
                }
                .run(step));
                let turn = self
                    .agent
                    .active_turn
                    .as_mut()
                    .expect("active turn checked before borrowing iteration fields");
                let mut consumer = IterationConsumer::new(turn);

                yield Ok(AgentEvent::Iteration(StreamPart::Delta(
                    IterationEvent::Started(iteration_id),
                )));

                let mut result = None;
                while let Some(item) = iteration.next().await {
                    let event = match item {
                        Ok(event) => event,
                        Err(error) => {
                            result = Some(Err(AgentError::from(error)));
                            break;
                        }
                    };
                    match event {
                        IterationLoopEvent::Iteration(IterationEvent::Llm(event)) => match consumer.consume_chat_event(event) {
                            Ok(Some(event)) => yield Ok(AgentEvent::Iteration(StreamPart::Delta(
                                IterationEvent::Llm(event),
                            ))),
                            Ok(None) => {}
                            Err(error) => {
                                result = Some(Err(error));
                                break;
                            }
                        },
                        IterationLoopEvent::Iteration(IterationEvent::BeforeToolCalls(calls)) => {
                            for event in consumer.finish_content() {
                                yield Ok(AgentEvent::Iteration(StreamPart::Delta(
                                    IterationEvent::Llm(event),
                                )));
                            }
                            if let Err(error) = consumer.finish_assistant() {
                                result = Some(Err(error));
                                break;
                            }
                            yield Ok(AgentEvent::Iteration(StreamPart::Delta(
                                IterationEvent::BeforeToolCalls(calls),
                            )));
                        }
                        IterationLoopEvent::Iteration(event) => {
                            yield Ok(AgentEvent::Iteration(StreamPart::Delta(event)));
                        }
                        IterationLoopEvent::ApprovalRequired {
                            tool_call_id,
                            tool_call,
                            reason,
                        } => {
                            yield Ok(AgentEvent::InputRequired(AgentInputRequest::Approval {
                                tool_call_id,
                                tool_call,
                                reason,
                            }));
                        }
                        IterationLoopEvent::ToolResult {
                            tool_call_id,
                            execution,
                        } => {
                            if let Err(error) = consumer.turn.record_tool_result(
                                &tool_call_id,
                                &execution.content,
                                !execution.ok,
                            ) {
                                result = Some(Err(AgentError::from(error)));
                                break;
                            }
                        }
                        IterationLoopEvent::Interrupted => {
                            result = Some(Ok(IterationCompletion::Interrupted));
                            break;
                        }
                        IterationLoopEvent::Cancelled => {
                            result = Some(Ok(IterationCompletion::Cancelled));
                            break;
                        }
                    }
                }

                for event in consumer.finish_content() {
                    yield Ok(AgentEvent::Iteration(StreamPart::Delta(
                        IterationEvent::Llm(event),
                    )));
                }
                let result = result.unwrap_or_else(|| consumer.finish_iteration());
                drop(consumer);
                drop(iteration);
                drop(permission);
                drop(tools);

                self.agent.refresh_recovery_state();
                yield Ok(AgentEvent::Iteration(StreamPart::End));

                match self.agent.reduce_iteration(result, &control) {
                    Ok(Some(event @ AgentEvent::Finished(_))) => {
                        yield Ok(event);
                        break;
                    }
                    Ok(Some(event)) => yield Ok(event),
                    Ok(None) => {}
                    Err(error) => {
                        yield Err(self.agent.fail(error));
                        break;
                    }
                }
            }
        }
    }
}

impl<H: ClawHttp, Timer: ClawTimer> Drop for ActiveRunGuard<'_, H, Timer> {
    fn drop(&mut self) {
        if self.agent.is_stopped() {
            return;
        }
        self.agent.abandon_open_task();
        self.agent.stop(StopReason::Cancelled);
    }
}

#[cfg(test)]
mod tests {
    use claw_interface::MemFs;
    use claw_memory::TranscriptStore;
    use serde_json::json;

    use super::*;

    #[test]
    fn streamed_output_reaches_the_transcript_before_progress() {
        let transcript = TranscriptStore::<MemFs>::in_memory(1);
        let mut turn = transcript.open_turn().expect("turn opens");
        turn.append_user("hello").expect("user fragment appends");
        turn.finish_user().expect("user message finishes");

        let events = [
            ChatStreamEvent::Reasoning(StreamPart::End),
            ChatStreamEvent::Output(StreamPart::Delta("Hel".to_owned())),
            ChatStreamEvent::Output(StreamPart::Delta("lo".to_owned())),
            ChatStreamEvent::Output(StreamPart::End),
            ChatStreamEvent::ToolCalls(StreamPart::End),
        ];
        let mut consumer = IterationConsumer::new(&mut turn);
        let mut actual = Vec::new();
        for event in events {
            let event = consumer
                .consume_chat_event(event)
                .expect("chat event is valid")
                .expect("test event is visible");
            match &event {
                ChatStreamEvent::Output(StreamPart::Delta(fragment)) if fragment == "Hel" => {
                    assert_eq!(assistant_content(&transcript).as_deref(), Some("Hel"));
                }
                ChatStreamEvent::Output(StreamPart::Delta(fragment)) if fragment == "lo" => {
                    assert_eq!(assistant_content(&transcript).as_deref(), Some("Hello"));
                }
                ChatStreamEvent::ToolCalls(StreamPart::End) => {
                    assert_eq!(assistant_content(&transcript).as_deref(), Some("Hello"));
                }
                _ => {}
            }
            actual.push(event);
        }
        actual.extend(consumer.finish_content());
        let completion = consumer.finish_iteration();
        drop(consumer);

        assert!(matches!(
            completion.expect("iteration is valid"),
            IterationCompletion::Response(text) if text == "Hello"
        ));
        assert_eq!(
            actual,
            vec![
                ChatStreamEvent::Reasoning(StreamPart::End),
                ChatStreamEvent::Output(StreamPart::Delta("Hel".to_owned())),
                ChatStreamEvent::Output(StreamPart::Delta("lo".to_owned())),
                ChatStreamEvent::Output(StreamPart::End),
                ChatStreamEvent::ToolCalls(StreamPart::End),
            ]
        );
        turn.discard();
    }

    #[test]
    fn assistant_draft_preserves_reasoning_text_and_tool_calls() {
        let transcript = TranscriptStore::<MemFs>::in_memory(2);
        let mut turn = transcript.open_turn().expect("turn opens");
        turn.append_user("hello").expect("user fragment appends");
        turn.finish_user().expect("user message finishes");
        let mut consumer = IterationConsumer::new(&mut turn);
        let events = [
            ChatStreamEvent::Reasoning(StreamPart::Delta("think".to_owned())),
            ChatStreamEvent::Reasoning(StreamPart::End),
            ChatStreamEvent::Output(StreamPart::Delta("answer".to_owned())),
            ChatStreamEvent::Output(StreamPart::End),
            ChatStreamEvent::ToolCalls(StreamPart::Delta(ToolCall {
                id: "call-1".to_owned(),
                name: "search".to_owned(),
                arguments_json: r#"{"query":"rust"}"#.to_owned(),
            })),
            ChatStreamEvent::ToolCalls(StreamPart::End),
        ];
        for event in events {
            consumer
                .consume_chat_event(event)
                .expect("assistant event is valid");
        }
        assert!(matches!(
            consumer.finish_iteration().expect("iteration finishes"),
            IterationCompletion::Tools
        ));
        drop(consumer);

        let turns = transcript.turns();
        assert_eq!(
            turns.last().expect("open turn is visible").messages[1],
            json!({
                "role": "assistant",
                "content": "answer",
                "reasoning_content": "think",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {
                        "name": "search",
                        "arguments": r#"{"query":"rust"}"#,
                    },
                }],
            })
        );
        turn.discard();
    }

    fn assistant_content(transcript: &TranscriptStore<MemFs>) -> Option<String> {
        let turns = transcript.turns();
        let assistant = turns.last()?.messages.get(1)?;
        assistant.get("content")?.as_str().map(str::to_owned)
    }
}

use alloc::{boxed::Box, collections::BTreeSet, string::String, sync::Arc, vec::Vec};

use barracuda_agent_context::{Block, BlockKind, Context};
use barracuda_agent_memory::{AssistantFragment, AssistantHandle, Transcript, TurnHandle};
use barracuda_agent_permission::{PermissionDecision, PermissionPolicy, PermissionRequest};
use barracuda_agent_persistence::DurableState;
use barracuda_agent_tool::ToolSet;
use barracuda_model_api::{ModelApi, RetryPolicy, ToolCall};
use barracuda_runtime_utils::stream::StreamPart;
use barracuda_runtime_utils::yield_stream::yield_stream;
use embedded_nal_async::{Dns, TcpConnect};
use futures_lite::StreamExt as _;
use getset::Getters;
use tracing::Instrument as _;

use crate::config::{ApiPurpose, SharedApiManager};
use crate::Message;

use super::context_provider::ContextProvider;
use super::effect::{AgentEffect, AgentEffectInbox};
use super::iteration_loop::{
    IterationEvent, IterationIdAllocator, IterationLoop, IterationLoopError, IterationLoopEvent,
    LlmStep, ToolAuthorization, ToolPermission, ToolPermissionPolicy, ToolPermissionRequest,
};
use super::stream::{
    AgentCompletion, AgentEngineEvent, AgentEngineStream, AgentError, AgentInputRequest,
    AgentIterationEvent, AgentOutcome, AgentSubmitError, ApprovalOutcome, RunControl,
};
use super::{AgentEngineState, TurnLifecycle};

/// All construction-time dependencies for one fully assembled AgentEngine.
pub(crate) struct AgentEngineConfig {
    pub(crate) state: DurableState<AgentEngineState>,
    pub(crate) transcript: Box<dyn Transcript>,
    pub(crate) agent_instruction: Block<'static>,
    pub(crate) inherited_context: Vec<Block<'static>>,
    pub(crate) context_providers: Vec<Box<dyn ContextProvider>>,
    pub(crate) api_manager: SharedApiManager,
    pub(crate) api_purpose: ApiPurpose,
    pub(crate) tools: ToolSet,
    pub(crate) effect_inbox: AgentEffectInbox,
    pub(crate) permission_policy: Arc<dyn PermissionPolicy>,
    pub(crate) retry_policy: RetryPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopReason {
    Ready,
    Completed,
    Interrupted,
    Cancelled,
    Failed,
}

enum RunState {
    Running,
    Stopped(StopReason),
}

enum IterationCompletion {
    Response(String),
    Tools,
    Interrupted,
    Cancelled,
}

struct ContextProviderEntry {
    provider: Box<dyn ContextProvider>,
    storage: super::AgentStorage,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AgentEngineBuildError {
    #[error("context provider id must not be empty")]
    InvalidContextProviderId,
    #[error("context provider id already exists: {0}")]
    DuplicateContextProviderId(String),
    #[error(transparent)]
    Tools(#[from] barracuda_agent_tool::ToolSetError),
}

/// One configured Agent and its complete single-Agent state machine.
#[derive(Getters)]
pub(crate) struct AgentEngine<Tcp: TcpConnect + 'static, Resolver: Dns + 'static> {
    state: DurableState<AgentEngineState>,
    llm: ModelApi<'static, Tcp, Resolver>,
    api_manager: SharedApiManager,
    api_purpose: ApiPurpose,
    retry_policy: RetryPolicy,
    transcript: Box<dyn Transcript>,
    active_turn: Option<TurnHandle>,
    tools: ToolSet,
    effect_inbox: AgentEffectInbox,
    permission_policy: Arc<dyn PermissionPolicy>,
    #[getset(get = "pub(crate)")]
    context: Context,
    run_state: RunState,
    iteration_id_allocator: IterationIdAllocator,
    context_providers: Vec<ContextProviderEntry>,
}

impl<Tcp: TcpConnect + 'static, Resolver: Dns + 'static> AgentEngine<Tcp, Resolver> {
    pub(crate) fn build(
        config: AgentEngineConfig,
        llm: ModelApi<'static, Tcp, Resolver>,
    ) -> Result<Self, AgentEngineBuildError> {
        let mut tools = config.tools;
        let mut context_providers = Vec::with_capacity(config.context_providers.len());
        let mut provider_ids = BTreeSet::new();
        for provider in config.context_providers {
            let id = provider.id();
            if id.is_empty() {
                return Err(AgentEngineBuildError::InvalidContextProviderId);
            }
            if !provider_ids.insert(id) {
                return Err(AgentEngineBuildError::DuplicateContextProviderId(id.into()));
            }
            let storage = super::AgentStorage::new(&config.state, id);
            if let Some(group) = provider.tools(&storage) {
                tools.add_group(group)?;
            }
            context_providers.push(ContextProviderEntry { provider, storage });
        }

        let mut context = Context::new();
        for block in &config.inherited_context {
            context.with(block.clone());
        }
        context.with(config.agent_instruction);

        Ok(Self {
            state: config.state,
            llm,
            api_manager: config.api_manager,
            api_purpose: config.api_purpose,
            retry_policy: config.retry_policy,
            transcript: config.transcript,
            active_turn: None,
            tools,
            effect_inbox: config.effect_inbox,
            permission_policy: config.permission_policy,
            context,
            run_state: RunState::Stopped(StopReason::Ready),
            iteration_id_allocator: IterationIdAllocator::new(),
            context_providers,
        })
    }

    /// Submit one message and borrow this Agent exclusively until its progress
    /// stream is dropped or reaches a terminal item.
    pub(crate) fn submit(
        &mut self,
        message: Message,
    ) -> Result<AgentEngineStream<'_>, AgentSubmitError> {
        self.begin(message)?;

        let control = AgentEngineStream::control();
        let stream = self.run_stream(control.clone());
        Ok(AgentEngineStream::new(stream, control))
    }

    fn run_stream(
        &mut self,
        control: RunControl,
    ) -> impl futures_core::Stream<Item = Result<AgentEngineEvent, AgentError>> + '_ {
        ActiveRunGuard::new(self).into_stream(control)
    }

    fn begin(&mut self, message: Message) -> Result<(), AgentSubmitError> {
        let RunState::Stopped(previous) = self.run_state else {
            return Err(AgentSubmitError::Running);
        };
        log::debug!("Agent message accepted: previous_state={previous:?}");
        tracing::debug!(name: "agent_message_accepted", previous = ?previous);
        self.iteration_id_allocator = IterationIdAllocator::new();
        let turn = self.transcript.open_turn()?;
        {
            let mut user = turn.user()?;
            user.append(message.as_str());
        }
        self.active_turn = Some(turn);
        self.run_state = RunState::Running;
        Ok(())
    }

    async fn reduce_iteration(
        &mut self,
        result: Result<IterationCompletion, AgentError>,
        control: &RunControl,
    ) -> Result<Option<AgentEngineEvent>, AgentError> {
        Ok(match result? {
            IterationCompletion::Response(text) => {
                if self.apply_continuations(control)? {
                    return Ok(None);
                }
                self.commit_active_turn().await?;
                self.stop(StopReason::Completed);
                Some(AgentEngineEvent::Finished(AgentOutcome::Completed(
                    AgentCompletion::Streamed(text),
                )))
            }
            IterationCompletion::Tools => {
                if let Some(event) = self.reduce_agent_effects().await? {
                    return Ok(Some(event));
                }
                if control.take_interrupt() {
                    self.abandon_open_task();
                    self.transcript.flush().await?;
                    self.stop(StopReason::Interrupted);
                    return Ok(Some(AgentEngineEvent::Finished(AgentOutcome::Interrupted)));
                }
                None
            }
            IterationCompletion::Interrupted => {
                self.abandon_open_task();
                self.transcript.flush().await?;
                self.stop(StopReason::Interrupted);
                Some(AgentEngineEvent::Finished(AgentOutcome::Interrupted))
            }
            IterationCompletion::Cancelled => {
                self.abandon_open_task();
                self.transcript.flush().await?;
                self.stop(StopReason::Cancelled);
                Some(AgentEngineEvent::Finished(AgentOutcome::Cancelled))
            }
        })
    }

    async fn reduce_agent_effects(&mut self) -> Result<Option<AgentEngineEvent>, AgentError> {
        let mut effects = self.effect_inbox.drain();
        if effects.len() > 1 {
            let count = effects.len();
            log::error!("Agent effect conflict: count={count}");
            tracing::error!(name: "agent_effect_conflict", count = count as u64);
            return Err(AgentError::ConflictingEffects { count });
        }
        match effects.pop() {
            Some(effect) => self.reduce_tool_effect(effect).await.map(Some),
            None => Ok(None),
        }
    }

    async fn reduce_tool_effect(
        &mut self,
        effect: AgentEffect,
    ) -> Result<AgentEngineEvent, AgentError> {
        let message = match effect {
            AgentEffect::Finish { final_message } => {
                self.finish_effect_assistant(&final_message)?;
                self.commit_active_turn().await?;
                self.stop(StopReason::Completed);
                final_message
            }
            AgentEffect::Yield { message } => {
                self.finish_effect_assistant(&message)?;
                self.commit_active_turn().await?;
                self.stop(StopReason::Completed);
                message
            }
        };
        Ok(AgentEngineEvent::Finished(AgentOutcome::Completed(
            AgentCompletion::EffectOutput(message),
        )))
    }

    fn apply_continuations(&mut self, control: &RunControl) -> Result<bool, AgentError> {
        let continuations = control.take_continuations();
        if continuations.is_empty() {
            return Ok(false);
        }
        let turn = self
            .active_turn
            .as_mut()
            .ok_or(AgentError::StateInvariant)?;
        for message in continuations {
            let mut user = turn.user()?;
            user.append(message.as_str());
        }
        Ok(true)
    }

    fn fail(&mut self, error: AgentError) -> AgentError {
        self.abandon_open_task();
        self.stop(StopReason::Failed);
        error
    }

    fn finish_effect_assistant(&mut self, message: &str) -> Result<(), AgentError> {
        let turn = self
            .active_turn
            .as_mut()
            .ok_or(AgentError::StateInvariant)?;
        let mut assistant = turn.assistant()?;
        assistant.append(AssistantFragment::Content(message));
        Ok(())
    }

    async fn commit_active_turn(&mut self) -> Result<(), AgentError> {
        drop(self.active_turn.take().ok_or(AgentError::StateInvariant)?);
        self.transcript.flush().await?;
        Ok(())
    }

    async fn prepare_provider_context(&mut self) -> Result<(), AgentError> {
        for entry in &mut self.context_providers {
            entry
                .provider
                .prepare(&entry.storage, self.transcript.as_ref())
                .await
                .map_err(AgentError::ContextProvider)?;
        }
        Ok(())
    }

    fn render_provider_context(&mut self) -> Result<Vec<serde_json::Value>, AgentError> {
        let mut sink = self.context.sink();
        for entry in &mut self.context_providers {
            entry
                .provider
                .contribute(&entry.storage, &mut sink)
                .map_err(AgentError::ContextProvider)?;
        }
        Ok(sink.into_history())
    }

    fn refresh_llm_config(&mut self) {
        let config = self.api_manager.borrow().get_api(self.api_purpose);
        if let Some(config) = config {
            if self.llm.set_config(config).is_err() {
                log::error!("invalid LLM configuration for {:?}", self.api_purpose);
                tracing::error!(name: "llm_config_invalid", purpose = ?self.api_purpose);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentPhase {
    Reasoning,
    Output,
    Ended,
}

#[derive(Default)]
struct AssistantDraft {
    output: String,
    response: Option<String>,
}

/// AgentEngine's consuming half of an iteration.
///
/// It is deliberately the only place that knows both transcript semantics and
/// owner-facing progress semantics: each stream part is incorporated into the
/// active turn before the corresponding progress item is forwarded.
struct IterationConsumer<'a> {
    turn: &'a TurnHandle,
    assistant: Option<AssistantHandle<'a>>,
    draft: AssistantDraft,
    phase: ContentPhase,
    assistant_finished: bool,
    saw_tool_calls: bool,
}

impl<'a> IterationConsumer<'a> {
    fn new(turn: &'a TurnHandle) -> Result<Self, AgentError> {
        let assistant = turn.assistant()?;
        Ok(Self {
            turn,
            assistant: Some(assistant),
            draft: AssistantDraft::default(),
            phase: ContentPhase::Reasoning,
            assistant_finished: false,
            saw_tool_calls: false,
        })
    }

    /// Apply one reasoning part before returning the matching owner-facing item.
    fn consume_reasoning(&mut self, part: StreamPart<String>) -> StreamPart<String> {
        debug_assert!(!self.assistant_finished);
        match part {
            StreamPart::Delta(fragment) => {
                debug_assert_eq!(self.phase, ContentPhase::Reasoning);
                if let Some(assistant) = self.assistant.as_mut() {
                    assistant.append(AssistantFragment::Reasoning(&fragment));
                }
                StreamPart::Delta(fragment)
            }
            StreamPart::End => {
                debug_assert_eq!(self.phase, ContentPhase::Reasoning);
                self.phase = ContentPhase::Output;
                StreamPart::End
            }
        }
    }

    /// Apply one output part to the transcript before returning it.
    fn consume_output(
        &mut self,
        part: StreamPart<String>,
    ) -> Result<StreamPart<String>, AgentError> {
        debug_assert!(!self.assistant_finished);
        match part {
            StreamPart::Delta(fragment) => {
                debug_assert_eq!(self.phase, ContentPhase::Output);
                if let Some(assistant) = self.assistant.as_mut() {
                    assistant.append(AssistantFragment::Content(&fragment));
                }
                self.draft.output.push_str(&fragment);
                Ok(StreamPart::Delta(fragment))
            }
            StreamPart::End => {
                debug_assert_eq!(self.phase, ContentPhase::Output);
                self.phase = ContentPhase::Ended;
                Ok(StreamPart::End)
            }
        }
    }

    fn finish_content(&mut self) -> Vec<AgentIterationEvent> {
        let mut progress = Vec::with_capacity(2);
        if self.phase == ContentPhase::Reasoning {
            progress.push(AgentIterationEvent::Reasoning(StreamPart::End));
            self.phase = ContentPhase::Output;
        }
        if self.phase == ContentPhase::Output {
            progress.push(AgentIterationEvent::Output(StreamPart::End));
            self.phase = ContentPhase::Ended;
        }
        progress
    }

    fn finish_iteration(&mut self) -> Result<IterationCompletion, AgentError> {
        self.finish_assistant(&[])?;
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

    fn finish_assistant(&mut self, calls: &[ToolCall]) -> Result<(), AgentError> {
        if self.assistant_finished {
            return Ok(());
        }
        let tool_calls = calls
            .iter()
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
            .collect::<Vec<_>>();
        self.saw_tool_calls = !tool_calls.is_empty();
        self.draft.response = tool_calls.is_empty().then(|| self.draft.output.clone());
        if let Some(mut assistant) = self.assistant.take() {
            for tool_call in tool_calls {
                assistant.append(AssistantFragment::ToolCall(tool_call));
            }
        }
        self.assistant_finished = true;
        Ok(())
    }
}

impl<Tcp: TcpConnect + 'static, Resolver: Dns + 'static> AgentEngine<Tcp, Resolver> {
    pub(crate) fn state(&self) -> &DurableState<AgentEngineState> {
        &self.state
    }

    pub(crate) fn is_stopped(&self) -> bool {
        matches!(self.run_state, RunState::Stopped(_))
    }

    fn stop(&mut self, reason: StopReason) {
        self.run_state = RunState::Stopped(reason);
        for entry in &mut self.context_providers {
            entry
                .provider
                .on_turn_lifecycle(&entry.storage, TurnLifecycle::Ended);
        }
    }

    fn abandon_open_task(&mut self) {
        self.effect_inbox.clear();
        drop(self.active_turn.take());
    }
}

struct EnginePermissionPolicy<'a> {
    policy: &'a dyn PermissionPolicy,
    control: &'a RunControl,
}

impl ToolPermissionPolicy for EnginePermissionPolicy<'_> {
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

/// Restores AgentEngine's stopped-state invariant if its borrowing stream is
/// dropped before producing a terminal event or error.
struct ActiveRunGuard<'a, Tcp: TcpConnect + 'static, Resolver: Dns + 'static> {
    agent: &'a mut AgentEngine<Tcp, Resolver>,
}

impl<'a, Tcp, Resolver> ActiveRunGuard<'a, Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    fn new(agent: &'a mut AgentEngine<Tcp, Resolver>) -> Self {
        Self { agent }
    }

    fn into_stream(
        self,
        control: RunControl,
    ) -> impl futures_core::Stream<Item = Result<AgentEngineEvent, AgentError>> + 'a {
        yield_stream(|yielder| async move {
            'agent_run: loop {
                if !matches!(self.agent.run_state, RunState::Running) {
                    yielder
                        .yield_one(Err(self.agent.fail(AgentError::StateInvariant)))
                        .await;
                    break;
                }
                if control.take_interrupt() {
                    self.agent.abandon_open_task();
                    self.agent.stop(StopReason::Interrupted);
                    yielder
                        .yield_one(Ok(AgentEngineEvent::Finished(AgentOutcome::Interrupted)))
                        .await;
                    break;
                }

                if let Err(error) = self.agent.apply_continuations(&control) {
                    yielder.yield_one(Err(self.agent.fail(error))).await;
                    break;
                }

                let iteration_id = self.agent.iteration_id_allocator.next();
                self.agent.refresh_llm_config();
                if self.agent.active_turn.is_none() {
                    yielder
                        .yield_one(Err(self.agent.fail(AgentError::StateInvariant)))
                        .await;
                    break;
                }

                let provider_count = self.agent.context_providers.len() as u64;
                let prepare_span = tracing::info_span!(
                    "iteration.prepare",
                    run.iteration = %iteration_id,
                    provider_count,
                );
                if let Err(error) = self
                    .agent
                    .prepare_provider_context()
                    .instrument(prepare_span.clone())
                    .await
                {
                    yielder.yield_one(Err(self.agent.fail(error))).await;
                    break;
                }

                let render_span =
                    prepare_span.in_scope(|| tracing::info_span!("context.render", provider_count));
                let history = match render_span.in_scope(|| self.agent.render_provider_context()) {
                    Ok(history) => history,
                    Err(error) => {
                        yielder.yield_one(Err(self.agent.fail(error))).await;
                        break 'agent_run;
                    }
                };
                let result = 'run_iteration: {
                    let tools = match render_span.in_scope(|| self.agent.tools.begin()) {
                        Ok(tools) => tools,
                        Err(error) => {
                            yielder
                                .yield_one(Err(self
                                    .agent
                                    .fail(AgentError::from(IterationLoopError::from(error)))))
                                .await;
                            break 'agent_run;
                        }
                    };
                    render_span.in_scope(|| {
                        self.agent
                            .context
                            .with(Block::new(BlockKind::StaticTools, tools.static_context()))
                            .with(Block::new(
                                BlockKind::DeferredTools,
                                tools.deferred_context(),
                            ))
                            .with_reminder(BlockKind::ToolReminder, Some(tools.reminders()));
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

                    let Some(turn) = self.agent.active_turn.as_mut() else {
                        yielder
                            .yield_one(Err(self.agent.fail(AgentError::StateInvariant)))
                            .await;
                        break 'agent_run;
                    };
                    let mut consumer = match IterationConsumer::new(turn) {
                        Ok(consumer) => consumer,
                        Err(error) => break 'run_iteration Err(error),
                    };
                    let permission = EnginePermissionPolicy {
                        policy: self.agent.permission_policy.as_ref(),
                        control: &control,
                    };
                    let mut iteration = Box::pin(
                        IterationLoop {
                            llm: &mut self.agent.llm,
                            control: &control,
                            permission: &permission,
                            retry: self.agent.retry_policy,
                        }
                        .run(step),
                    );
                    let iteration_span = tracing::info_span!(
                        "iteration_loop",
                        run.iteration = %iteration_id,
                    );

                    yielder
                        .yield_one(Ok(AgentEngineEvent::Iteration(StreamPart::Delta(
                            AgentIterationEvent::Started(iteration_id),
                        ))))
                        .await;

                    let mut result = None;
                    let mut tool_results_ended = false;
                    while let Some(item) = iteration.next().instrument(iteration_span.clone()).await
                    {
                        let event = match item {
                            Ok(event) => event,
                            Err(error) => {
                                result = Some(Err(AgentError::from(error)));
                                break;
                            }
                        };
                        match event {
                            IterationLoopEvent::Iteration(IterationEvent::Reasoning(part)) => {
                                let part = consumer.consume_reasoning(part);
                                yielder
                                    .yield_one(Ok(AgentEngineEvent::Iteration(StreamPart::Delta(
                                        AgentIterationEvent::Reasoning(part),
                                    ))))
                                    .await;
                            }
                            IterationLoopEvent::Iteration(IterationEvent::Output(part)) => {
                                match consumer.consume_output(part) {
                                    Ok(part) => {
                                        yielder
                                            .yield_one(Ok(AgentEngineEvent::Iteration(
                                                StreamPart::Delta(AgentIterationEvent::Output(
                                                    part,
                                                )),
                                            )))
                                            .await
                                    }
                                    Err(error) => {
                                        result = Some(Err(error));
                                        break;
                                    }
                                }
                            }
                            #[cfg(feature = "cache_profile")]
                            IterationLoopEvent::Iteration(IterationEvent::Usage(usage)) => {
                                yielder
                                    .yield_one(Ok(AgentEngineEvent::Iteration(StreamPart::Delta(
                                        AgentIterationEvent::Usage(usage),
                                    ))))
                                    .await;
                            }
                            IterationLoopEvent::Iteration(IterationEvent::BeforeToolCalls(
                                calls,
                            )) => {
                                for event in consumer.finish_content() {
                                    yielder
                                        .yield_one(Ok(AgentEngineEvent::Iteration(
                                            StreamPart::Delta(event),
                                        )))
                                        .await;
                                }
                                if let Err(error) = consumer.finish_assistant(&calls) {
                                    result = Some(Err(error));
                                    break;
                                }
                                self.agent.state.get_mut().record_inflight_toolcalls(calls);
                                futures_lite::future::yield_now().await;
                            }
                            IterationLoopEvent::Iteration(IterationEvent::ToolResult(part)) => {
                                match part {
                                    StreamPart::Delta((call, output)) => {
                                        if tool_results_ended {
                                            result = Some(Err(AgentError::StateInvariant));
                                            break;
                                        }
                                        match consumer.turn.tool(&call.id, !output.ok) {
                                            Ok(mut tool) => tool.append(&output.content),
                                            Err(error) => {
                                                result = Some(Err(AgentError::from(error)));
                                                break;
                                            }
                                        }
                                        self.agent
                                            .state
                                            .get_mut()
                                            .remove_inflight_toolcall(&call.id);
                                        yielder
                                            .yield_one(Ok(AgentEngineEvent::Iteration(
                                                StreamPart::Delta(AgentIterationEvent::ToolResult(
                                                    StreamPart::Delta((call, output)),
                                                )),
                                            )))
                                            .await;
                                    }
                                    StreamPart::End => {
                                        if tool_results_ended {
                                            result = Some(Err(AgentError::StateInvariant));
                                            break;
                                        }
                                        for event in consumer.finish_content() {
                                            yielder
                                                .yield_one(Ok(AgentEngineEvent::Iteration(
                                                    StreamPart::Delta(event),
                                                )))
                                                .await;
                                        }
                                        if let Err(error) = consumer.finish_assistant(&[]) {
                                            result = Some(Err(error));
                                            break;
                                        }
                                        tool_results_ended = true;
                                        yielder
                                            .yield_one(Ok(AgentEngineEvent::Iteration(
                                                StreamPart::Delta(AgentIterationEvent::ToolResult(
                                                    StreamPart::End,
                                                )),
                                            )))
                                            .await;
                                    }
                                }
                            }
                            IterationLoopEvent::Detached(handle) => {
                                yielder
                                    .yield_one(Ok(AgentEngineEvent::Detached(handle)))
                                    .await;
                            }
                            IterationLoopEvent::ApprovalRequired {
                                tool_call_id,
                                tool_call,
                                reason,
                            } => {
                                yielder
                                    .yield_one(Ok(AgentEngineEvent::InputRequired(
                                        AgentInputRequest::Approval {
                                            tool_call_id,
                                            tool_call,
                                            reason,
                                        },
                                    )))
                                    .await;
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
                        yielder
                            .yield_one(Ok(AgentEngineEvent::Iteration(StreamPart::Delta(event))))
                            .await;
                    }
                    if !tool_results_ended {
                        yielder
                            .yield_one(Ok(AgentEngineEvent::Iteration(StreamPart::Delta(
                                AgentIterationEvent::ToolResult(StreamPart::End),
                            ))))
                            .await;
                    }
                    result.unwrap_or_else(|| consumer.finish_iteration())
                };

                yielder
                    .yield_one(Ok(AgentEngineEvent::Iteration(StreamPart::End)))
                    .await;

                match self.agent.reduce_iteration(result, &control).await {
                    Ok(Some(event @ AgentEngineEvent::Finished(_))) => {
                        yielder.yield_one(Ok(event)).await;
                        break;
                    }
                    Ok(Some(event)) => yielder.yield_one(Ok(event)).await,
                    Ok(None) => {}
                    Err(error) => {
                        yielder.yield_one(Err(self.agent.fail(error))).await;
                        break;
                    }
                }
            }
        })
    }
}

impl<Tcp: TcpConnect + 'static, Resolver: Dns + 'static> Drop
    for ActiveRunGuard<'_, Tcp, Resolver>
{
    fn drop(&mut self) {
        if self.agent.is_stopped() {
            return;
        }
        self.agent.abandon_open_task();
        self.agent.stop(StopReason::Cancelled);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use barracuda_agent_memory::TranscriptStore;
    use barracuda_platform_test::memory_vfs;
    use futures_lite::future::block_on;
    use serde_json::json;

    use super::*;

    #[test]
    fn streamed_output_reaches_the_transcript_before_progress() {
        let transcript = transcript(1);
        let turn = transcript.open_turn().expect("turn opens");
        {
            let mut user = turn.user().expect("user message opens");
            user.append("hello");
        }

        let mut consumer = IterationConsumer::new(&turn).expect("assistant message opens");
        let mut actual = vec![AgentIterationEvent::Reasoning(
            consumer.consume_reasoning(StreamPart::End),
        )];
        let first = consumer
            .consume_output(StreamPart::Delta("Hel".to_owned()))
            .expect("output fragment appends");
        assert_eq!(assistant_content(&transcript).as_deref(), Some("Hel"));
        actual.push(AgentIterationEvent::Output(first));
        let second = consumer
            .consume_output(StreamPart::Delta("lo".to_owned()))
            .expect("output fragment appends");
        assert_eq!(assistant_content(&transcript).as_deref(), Some("Hello"));
        actual.push(AgentIterationEvent::Output(second));
        actual.push(AgentIterationEvent::Output(
            consumer
                .consume_output(StreamPart::End)
                .expect("output ends"),
        ));
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
                AgentIterationEvent::Reasoning(StreamPart::End),
                AgentIterationEvent::Output(StreamPart::Delta("Hel".to_owned())),
                AgentIterationEvent::Output(StreamPart::Delta("lo".to_owned())),
                AgentIterationEvent::Output(StreamPart::End),
            ]
        );
        drop(turn);
    }

    #[test]
    fn assistant_draft_preserves_reasoning_text_and_tool_calls() {
        let transcript = transcript(2);
        let turn = transcript.open_turn().expect("turn opens");
        {
            let mut user = turn.user().expect("user message opens");
            user.append("hello");
        }
        let mut consumer = IterationConsumer::new(&turn).expect("assistant message opens");
        consumer.consume_reasoning(StreamPart::Delta("think".to_owned()));
        consumer.consume_reasoning(StreamPart::End);
        consumer
            .consume_output(StreamPart::Delta("answer".to_owned()))
            .expect("output appends");
        consumer
            .consume_output(StreamPart::End)
            .expect("output ends");
        consumer
            .finish_assistant(&[ToolCall {
                id: "call-1".to_owned(),
                name: "search".to_owned(),
                arguments_json: r#"{"query":"rust"}"#.to_owned(),
            }])
            .expect("assistant finishes");
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
        drop(turn);
    }

    #[test]
    fn long_reasoning_delta_is_forwarded_in_full() {
        let transcript = transcript(3);
        let turn = transcript.open_turn().expect("turn opens");
        {
            let mut user = turn.user().expect("user message opens");
            user.append("hello");
        }

        let mut consumer = IterationConsumer::new(&turn).expect("assistant message opens");
        let reasoning = "界".repeat(1_000);

        assert_eq!(
            consumer.consume_reasoning(StreamPart::Delta(reasoning.clone())),
            StreamPart::Delta(reasoning)
        );
    }

    fn transcript(id: u32) -> TranscriptStore {
        block_on(async {
            TranscriptStore::new(
                memory_vfs().await.expect("memory VFS mounts"),
                id,
                "/transcript",
            )
            .await
            .expect("in-memory transcript opens")
        })
    }

    fn assistant_content(transcript: &TranscriptStore) -> Option<String> {
        let turns = transcript.turns();
        let assistant = turns.last()?.messages.get(1)?;
        assistant.get("content")?.as_str().map(str::to_owned)
    }
}

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::{BTreeMap, HashSet, VecDeque};

use claw_api::{ChatRequest, LlmDelta, LlmResponse, ToolCall};
use claw_interface::http::StreamingHttp;
use claw_interface::{Cancel, ClawHttp, ClawTimer};
use claw_tool::{RawToolInvocation, ToolExecution, ToolExecutor, ToolInvocation, ToolSetHandle};
use futures_lite::{future, StreamExt};
use tracing::Instrument as _;

use super::super::stream::{AgentProgress, ProgressEmitter};
use super::types::{
    AppendedMessages, IterationLoopError, IterationOutcome, IterationResult, LlmStep,
};
use super::{
    IterationLoop, PendingToolPermission, ToolAuthorization, ToolCallId, ToolCallIdAllocator,
    ToolPermission, ToolPermissionPolicy, ToolPermissionRequest,
};
use crate::protocol::InflightToolCall;

struct IterationOutput<'a> {
    progress: &'a ProgressEmitter,
    phase: ContentPhase,
    reasoning_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentPhase {
    Reasoning,
    Output,
    ToolCalls,
    Ended,
}

struct PreparedToolCall {
    id: ToolCallId,
    provider_id: String,
    name: String,
    arguments_json: String,
}

impl PreparedToolCall {
    fn invocation(&self) -> Result<ToolInvocation<'_>, IterationLoopError> {
        ToolInvocation::try_from(RawToolInvocation {
            id: Some(&self.provider_id),
            name: &self.name,
            arguments_json: &self.arguments_json,
        })
        .map_err(|_| IterationLoopError::MalformedToolCall)
    }
}

struct ToolCallResult {
    content: String,
    ok: bool,
}

struct ToolCallIdentity {
    id: ToolCallId,
    provider_id: String,
}

struct PendingToolCall<'a> {
    call: PreparedToolCall,
    permission: PendingToolPermission<'a>,
}

type ToolRunOutput = Result<(ToolCallId, ToolCallResult), IterationLoopError>;
type ToolRunFuture<'a> = Pin<Box<dyn Future<Output = ToolRunOutput> + 'a>>;

struct ToolRuns<'a> {
    futures: Vec<ToolRunFuture<'a>>,
    cursor: usize,
}

struct ToolCallBatch {
    messages: AppendedMessages,
    order: Vec<ToolCallIdentity>,
    results: BTreeMap<ToolCallId, ToolCallResult>,
}

enum ToolBatchUpdate {
    Permission(ToolPermission),
    Execution(ToolRunOutput),
}

impl<'a> IterationOutput<'a> {
    fn new(progress: &'a ProgressEmitter) -> Self {
        Self {
            progress,
            phase: ContentPhase::Reasoning,
            reasoning_bytes: 0,
        }
    }

    async fn emit_reasoning(&mut self, fragment: &str) {
        debug_assert_eq!(self.phase, ContentPhase::Reasoning);
        if fragment.is_empty() || self.reasoning_bytes >= reasoning_limit() {
            return;
        }
        let remaining = reasoning_limit() - self.reasoning_bytes;
        let mut end = remaining.min(fragment.len());
        while end > 0 && !fragment.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            return;
        }
        self.reasoning_bytes += end;
        self.progress
            .send(AgentProgress::ReasoningDelta(fragment[..end].to_owned()))
            .await;
    }

    async fn emit_output(&mut self, text: String) {
        self.finish_reasoning().await;
        debug_assert_eq!(self.phase, ContentPhase::Output);
        self.progress.send(AgentProgress::OutputDelta(text)).await;
    }

    async fn emit_tool_call(&mut self, call: ToolCall) {
        self.finish_reasoning().await;
        self.finish_output().await;
        debug_assert_eq!(self.phase, ContentPhase::ToolCalls);
        self.progress.send(AgentProgress::ToolCall(call)).await;
    }

    async fn finish(&mut self) {
        self.finish_reasoning().await;
        self.finish_output().await;
        self.finish_tool_calls().await;
    }

    async fn finish_reasoning(&mut self) {
        if self.phase == ContentPhase::Reasoning {
            self.progress.send(AgentProgress::ReasoningEnded).await;
            self.phase = ContentPhase::Output;
        }
    }

    async fn finish_output(&mut self) {
        if self.phase == ContentPhase::Output {
            self.progress.send(AgentProgress::OutputEnded).await;
            self.phase = ContentPhase::ToolCalls;
        }
    }

    async fn finish_tool_calls(&mut self) {
        if self.phase == ContentPhase::ToolCalls {
            self.progress.send(AgentProgress::ToolCallsEnded).await;
            self.phase = ContentPhase::Ended;
        }
    }
}

impl<H, Timer, P> IterationLoop<'_, H, Timer, P>
where
    H: ClawHttp + StreamingHttp,
    Timer: ClawTimer,
    P: ToolPermissionPolicy,
{
    pub(crate) async fn run(self, step: LlmStep<'_>) -> IterationResult {
        let span = tracing::info_span!("iteration_loop", run.iteration = %step.iteration_id);
        run_one_iteration(self, step).instrument(span).await
    }
}

async fn run_one_iteration<H, Timer, P>(
    mut loop_: IterationLoop<'_, H, Timer, P>,
    step: LlmStep<'_>,
) -> IterationResult
where
    H: ClawHttp + StreamingHttp,
    Timer: ClawTimer,
    P: ToolPermissionPolicy,
{
    loop_
        .progress
        .send(AgentProgress::IterationStarted(step.iteration_id))
        .await;
    let response = {
        let mut output = IterationOutput::new(loop_.progress);
        let response = call_llm(&mut loop_, &step, &mut output).await;
        output.finish().await;
        response
    };

    let result = match response {
        Ok(Some(response)) if response.tool_calls.is_empty() => {
            Ok(IterationOutcome::Response(response))
        }
        Ok(Some(response)) => {
            loop_
                .progress
                .send(AgentProgress::ToolCalls(observable_tool_calls(&response)))
                .await;
            execute_tool_calls(&response, step.tools, loop_.control, loop_.permission).await
        }
        Ok(None) => Ok(IterationOutcome::Cancelled(AppendedMessages::empty())),
        Err(error) => Err(error),
    };

    loop_.progress.send(AgentProgress::IterationEnded).await;
    result
}

async fn call_llm<H, Timer, P>(
    loop_: &mut IterationLoop<'_, H, Timer, P>,
    step: &LlmStep<'_>,
    output: &mut IterationOutput<'_>,
) -> Result<Option<LlmResponse>, IterationLoopError>
where
    H: ClawHttp + StreamingHttp,
    Timer: ClawTimer,
{
    if loop_.control.is_cancelled() {
        tracing::warn!(name: "cancelled", checkpoint = "before_llm_http");
        return Ok(None);
    }

    let chat_request = ChatRequest {
        system_prompt: step.system_prompt,
        messages: step.messages,
        reminders: step.reminders,
        tools_json: Some(step.tools.schemas_json()),
        retry: loop_.retry,
    };
    let cancel = Cancel::new(loop_.control.cancel_flag());
    let max_attempts = 1_u64;
    let chat_span = tracing::info_span!("api.chat", purpose = "iteration", max_attempts);

    let stream_result = loop_
        .llm
        .chat_stream(&chat_request, cancel)
        .instrument(chat_span.clone())
        .await;
    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => return interpret_chat_error(loop_.control.is_cancelled(), error),
    };

    loop {
        let next = StreamExt::next(&mut stream)
            .instrument(chat_span.clone())
            .await;
        match next {
            Some(Ok(LlmDelta::Reasoning(text))) => output.emit_reasoning(&text).await,
            Some(Ok(LlmDelta::Output(text))) => output.emit_output(text).await,
            Some(Ok(LlmDelta::ToolCall {
                id,
                name,
                arguments,
                ..
            })) => {
                output
                    .emit_tool_call(ToolCall {
                        id,
                        name,
                        arguments_json: arguments,
                    })
                    .await;
            }
            Some(Err(error)) => {
                return interpret_chat_error(loop_.control.is_cancelled(), error);
            }
            None => break,
        }
    }

    let response = match stream.take_response() {
        Some(Ok(response)) => response,
        Some(Err(error)) => return interpret_chat_error(loop_.control.is_cancelled(), error),
        None => {
            return interpret_chat_error(
                loop_.control.is_cancelled(),
                claw_api::ChatError::truncated_stream(),
            );
        }
    };

    #[cfg(feature = "cache_profile")]
    if let Some(usage) = response.usage {
        output.progress.send(AgentProgress::Usage(usage)).await;
    }

    if loop_.control.is_cancelled() {
        tracing::warn!(name: "cancelled", checkpoint = "after_llm");
        Ok(None)
    } else {
        Ok(Some(response))
    }
}

async fn execute_tool_calls<P>(
    response: &LlmResponse,
    tools: &ToolSetHandle<'_>,
    control: &super::super::stream::RunControl,
    permission: &P,
) -> IterationResult
where
    P: ToolPermissionPolicy,
{
    let mut batch = ToolCallBatch::new(response)?;
    let mut provider_ids = HashSet::with_capacity(response.tool_calls.len());
    let mut tool_call_ids = ToolCallIdAllocator::new();
    let mut calls = Vec::with_capacity(response.tool_calls.len());
    for tool_call in &response.tool_calls {
        if tool_call.id.is_empty() {
            return Err(IterationLoopError::MissingProviderToolCallId);
        }
        if !provider_ids.insert(tool_call.id.as_str()) {
            return Err(IterationLoopError::DuplicateProviderToolCallId(
                tool_call.id.clone(),
            ));
        }
        let id = tool_call_ids.next();
        batch.order.push(ToolCallIdentity {
            id,
            provider_id: tool_call.id.clone(),
        });
        calls.push((id, tool_call));
    }

    let executor = ToolExecutor::new(tools);
    let mut runs = ToolRuns::new();
    let mut pending = VecDeque::new();
    for (id, tool_call) in calls {
        if control.is_cancelled() {
            return Ok(IterationOutcome::Cancelled(batch.into_messages()));
        }
        let invocation = match ToolInvocation::try_from(RawToolInvocation {
            id: Some(&tool_call.id),
            name: &tool_call.name,
            arguments_json: &tool_call.arguments_json,
        }) {
            Ok(invocation) => invocation,
            Err(error) => {
                batch.collect(id, error.to_string(), false);
                continue;
            }
        };
        let action = match tools.classify(&invocation) {
            Ok(action) => action,
            Err(error) => {
                batch.collect(id, error.to_string(), false);
                continue;
            }
        };
        let prepared = PreparedToolCall {
            id,
            provider_id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments_json: invocation.arguments_json().to_owned(),
        };

        match permission.authorize(ToolPermissionRequest {
            tool_call_id: id,
            action: &action,
        }) {
            ToolAuthorization::Allow => runs.push(execute_prepared_tool(&executor, prepared)),
            ToolAuthorization::Deny(reason) => batch.collect(id, reason, false),
            ToolAuthorization::Pending(permission) => {
                pending.push_back(PendingToolCall {
                    call: prepared,
                    permission,
                });
            }
        }
    }

    let mut active_permission = pending.pop_front();
    while active_permission.is_some() || !runs.is_empty() {
        if control.is_cancelled() {
            return Ok(IterationOutcome::Cancelled(batch.into_messages()));
        }

        let update = match active_permission.as_mut() {
            Some(waiting) if !runs.is_empty() => {
                future::or(
                    async { ToolBatchUpdate::Permission(waiting.permission.as_mut().await) },
                    async { ToolBatchUpdate::Execution(runs.next().await) },
                )
                .await
            }
            Some(waiting) => ToolBatchUpdate::Permission(waiting.permission.as_mut().await),
            None => ToolBatchUpdate::Execution(runs.next().await),
        };

        match update {
            ToolBatchUpdate::Execution(result) => {
                let (id, result) = result?;
                batch.collect(id, result.content, result.ok);
            }
            ToolBatchUpdate::Permission(decision) => {
                let Some(waiting) = active_permission.take() else {
                    return Err(IterationLoopError::IncompleteToolBatch);
                };
                match decision {
                    ToolPermission::Allow => {
                        runs.push(execute_prepared_tool(&executor, waiting.call));
                    }
                    ToolPermission::Deny(reason) => {
                        batch.collect(waiting.call.id, reason, false);
                    }
                    ToolPermission::Interrupted => return Ok(IterationOutcome::Interrupted),
                    ToolPermission::Cancelled => {
                        return Ok(IterationOutcome::Cancelled(batch.into_messages()));
                    }
                }
                active_permission = pending.pop_front();
            }
        }
    }

    if control.is_cancelled() {
        return Ok(IterationOutcome::Cancelled(batch.into_messages()));
    }
    if !batch.is_complete() {
        return Err(IterationLoopError::IncompleteToolBatch);
    }
    Ok(IterationOutcome::Tools(batch.into_messages()))
}

impl ToolCallBatch {
    fn new(response: &LlmResponse) -> Result<Self, IterationLoopError> {
        let mut messages = AppendedMessages::empty();
        append_assistant_tool_calls(&mut messages, response)?;
        Ok(Self {
            messages,
            order: Vec::with_capacity(response.tool_calls.len()),
            results: BTreeMap::new(),
        })
    }

    fn collect(&mut self, id: ToolCallId, content: String, ok: bool) {
        let previous = self.results.insert(id, ToolCallResult { content, ok });
        debug_assert!(previous.is_none());
    }

    fn is_complete(&self) -> bool {
        self.results.len() == self.order.len()
            && self
                .order
                .iter()
                .all(|call| self.results.contains_key(&call.id))
    }

    fn into_messages(mut self) -> AppendedMessages {
        for call in self.order {
            let Some(result) = self.results.remove(&call.id) else {
                continue;
            };
            self.messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": call.provider_id,
                "content": result.content,
                "is_error": !result.ok,
            }));
        }
        self.messages
    }
}

impl<'a> ToolRuns<'a> {
    fn new() -> Self {
        Self {
            futures: Vec::new(),
            cursor: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.futures.is_empty()
    }

    fn push(&mut self, run: impl Future<Output = ToolRunOutput> + 'a) {
        self.futures.push(Box::pin(run));
    }

    async fn next(&mut self) -> ToolRunOutput {
        future::poll_fn(|context| self.poll_next(context)).await
    }

    fn poll_next(&mut self, context: &mut Context<'_>) -> Poll<ToolRunOutput> {
        let count = self.futures.len();
        if count == 0 {
            return Poll::Ready(Err(IterationLoopError::IncompleteToolBatch));
        }

        let start = self.cursor % count;
        for offset in 0..count {
            let index = (start + offset) % count;
            if let Poll::Ready(output) = self.futures[index].as_mut().poll(context) {
                drop(self.futures.swap_remove(index));
                self.cursor = if self.futures.is_empty() {
                    0
                } else {
                    index % self.futures.len()
                };
                return Poll::Ready(output);
            }
        }

        self.cursor = (start + 1) % count;
        Poll::Pending
    }
}

async fn execute_prepared_tool(
    executor: &ToolExecutor<'_>,
    call: PreparedToolCall,
) -> ToolRunOutput {
    let id = call.id;
    let span = tracing::info_span!("toolcall", tool = %call.name);
    let invocation = call.invocation()?;
    let ToolExecution { content, ok } =
        executor.execute(&invocation).instrument(span.clone()).await;
    span.in_scope(|| {
        if ok {
            tracing::info!(name: "result", ok);
        } else {
            tracing::warn!(name: "result", ok);
        }
    });
    Ok((id, ToolCallResult { content, ok }))
}

fn append_assistant_tool_calls(
    messages: &mut AppendedMessages,
    response: &LlmResponse,
) -> Result<(), IterationLoopError> {
    let Some(raw) = response
        .raw_message_json
        .as_deref()
        .filter(|message| !message.is_empty())
    else {
        return Err(IterationLoopError::MissingAssistantMessage);
    };
    let assistant =
        serde_json::from_str(raw).map_err(|_| IterationLoopError::MalformedAssistantMessage)?;
    messages.push(assistant);
    Ok(())
}

fn observable_tool_calls(response: &LlmResponse) -> Vec<InflightToolCall> {
    response
        .tool_calls
        .iter()
        .filter_map(|call| {
            let invocation = ToolInvocation::try_from(RawToolInvocation {
                id: Some(&call.id),
                name: &call.name,
                arguments_json: &call.arguments_json,
            })
            .ok()?;
            Some(InflightToolCall::new(
                invocation.name(),
                invocation.arguments_value().unwrap_or_else(|_| {
                    serde_json::Value::String(invocation.arguments_json().to_owned())
                }),
            ))
        })
        .collect()
}

fn interpret_chat_error(
    cancelled: bool,
    error: claw_api::ChatError,
) -> Result<Option<LlmResponse>, IterationLoopError> {
    if cancelled || error.is_aborted() {
        tracing::warn!(name: "cancelled", checkpoint = "in_llm_http_abort");
        Ok(None)
    } else {
        tracing::error!(name: "chat_failed", kind = "chat");
        Err(IterationLoopError::Chat(error))
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

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use claw_permission::{Action, AllowAll, RiskClass};
    use claw_tool::{
        SyncToolHandler, Tool, ToolGroup, ToolOutput, ToolRegistry, ToolResult, ToolSpec,
    };
    use futures_lite::future::block_on;
    use serde_json::json;

    use super::*;
    use crate::agent::base_agent::stream::AgentStreamHandle;

    struct CountingTool {
        name: &'static str,
        calls: Arc<AtomicUsize>,
    }

    impl ToolSpec for CountingTool {
        fn name(&self) -> &str {
            self.name
        }

        fn schema(&self) -> &str {
            "{}"
        }

        fn classify(&self, _call: &ToolInvocation<'_>) -> Action {
            Action::new(self.name, RiskClass::Safe)
        }
    }

    impl SyncToolHandler for CountingTool {
        fn invoke(&self, _call: &ToolInvocation<'_>) -> ToolResult<ToolOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput {
                output: self.name.to_owned(),
                ok: true,
            })
        }
    }

    struct SelectivePermission {
        executions: Arc<AtomicUsize>,
        checks: Cell<usize>,
        ids: RefCell<Vec<ToolCallId>>,
    }

    impl ToolPermissionPolicy for SelectivePermission {
        fn authorize<'a>(&'a self, request: ToolPermissionRequest<'_>) -> ToolAuthorization<'a> {
            assert_eq!(
                self.executions.load(Ordering::SeqCst),
                0,
                "every tool is classified before execution starts"
            );
            self.checks.set(self.checks.get() + 1);
            self.ids.borrow_mut().push(request.tool_call_id);
            if request.action.verb() == "denied" {
                let executions = Arc::clone(&self.executions);
                ToolAuthorization::Pending(Box::pin(async move {
                    while executions.load(Ordering::SeqCst) == 0 {
                        futures_lite::future::yield_now().await;
                    }
                    ToolPermission::Deny("policy denied".to_owned())
                }))
            } else {
                ToolAuthorization::Allow
            }
        }
    }

    #[test]
    fn allowed_tools_run_while_permission_is_pending_and_results_keep_call_order() {
        let executions = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(ToolRegistry::new());
        let mut tool_set = registry.tool_set();
        tool_set
            .add_group(ToolGroup::new(
                "test",
                true,
                [
                    Tool::from_sync(CountingTool {
                        name: "allowed",
                        calls: Arc::clone(&executions),
                    }),
                    Tool::from_sync(CountingTool {
                        name: "denied",
                        calls: Arc::clone(&executions),
                    }),
                ],
            ))
            .expect("test tools are valid");
        let tools = tool_set.begin().expect("test tool set starts");
        let response = response([
            ToolCall {
                id: "call-deny".to_owned(),
                name: "denied".to_owned(),
                arguments_json: "{}".to_owned(),
            },
            ToolCall {
                id: "call-allow".to_owned(),
                name: "allowed".to_owned(),
                arguments_json: "{}".to_owned(),
            },
        ]);
        let control = AgentStreamHandle::control();
        let permission = SelectivePermission {
            executions: Arc::clone(&executions),
            checks: Cell::new(0),
            ids: RefCell::new(Vec::new()),
        };

        let result = block_on(execute_tool_calls(&response, &tools, &control, &permission))
            .expect("tool calls complete");
        let IterationOutcome::Tools(messages) = result else {
            panic!("iteration should produce tool messages");
        };

        assert_eq!(permission.checks.get(), 2);
        assert_eq!(
            permission.ids.into_inner(),
            [ToolCallId::new(0), ToolCallId::new(1)]
        );
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(
            messages.into_json_array(),
            json!([
                {
                    "role": "assistant",
                    "tool_calls": [
                        { "id": "call-deny" },
                        { "id": "call-allow" }
                    ]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call-deny",
                    "content": "policy denied",
                    "is_error": true
                },
                {
                    "role": "tool",
                    "tool_call_id": "call-allow",
                    "content": "allowed",
                    "is_error": false
                }
            ])
        );
    }

    #[test]
    fn yolo_policy_rejects_duplicate_provider_ids_before_execution() {
        let executions = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(ToolRegistry::new());
        let mut tool_set = registry.tool_set();
        tool_set
            .add_group(ToolGroup::new(
                "test",
                true,
                [Tool::from_sync(CountingTool {
                    name: "allowed",
                    calls: Arc::clone(&executions),
                })],
            ))
            .expect("test tool is valid");
        let tools = tool_set.begin().expect("test tool set starts");
        let response = response([
            ToolCall {
                id: "duplicate".to_owned(),
                name: "allowed".to_owned(),
                arguments_json: "{}".to_owned(),
            },
            ToolCall {
                id: "duplicate".to_owned(),
                name: "allowed".to_owned(),
                arguments_json: "{}".to_owned(),
            },
        ]);
        let control = AgentStreamHandle::control();
        let permission = AllowAll;

        let error = block_on(execute_tool_calls(&response, &tools, &control, &permission))
            .expect_err("duplicate ids fail the iteration");

        assert_eq!(
            error,
            IterationLoopError::DuplicateProviderToolCallId("duplicate".to_owned())
        );
        assert_eq!(executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn iteration_output_closes_skipped_content_streams_in_order() {
        let (progress, receiver) = AgentStreamHandle::channel();
        let producer = async {
            let mut output = IterationOutput::new(&progress);
            output.emit_reasoning("thinking").await;
            output
                .emit_tool_call(ToolCall {
                    id: "call-1".to_owned(),
                    name: "search".to_owned(),
                    arguments_json: r#"{"query":"rust"}"#.to_owned(),
                })
                .await;
            output.finish().await;
        };
        let consumer = async {
            let mut actual = Vec::new();
            for _ in 0..5 {
                let envelope = receiver.recv().await.expect("output remains open");
                actual.push(envelope.progress);
                envelope
                    .resume
                    .send(())
                    .await
                    .expect("producer remains open");
            }
            actual
        };

        let (_, actual) = block_on(futures_lite::future::zip(producer, consumer));
        assert_eq!(
            actual,
            vec![
                AgentProgress::ReasoningDelta("thinking".to_owned()),
                AgentProgress::ReasoningEnded,
                AgentProgress::OutputEnded,
                AgentProgress::ToolCall(ToolCall {
                    id: "call-1".to_owned(),
                    name: "search".to_owned(),
                    arguments_json: r#"{"query":"rust"}"#.to_owned(),
                }),
                AgentProgress::ToolCallsEnded,
            ]
        );
    }

    fn response<const N: usize>(tool_calls: [ToolCall; N]) -> LlmResponse {
        LlmResponse {
            raw_message_json: Some(
                json!({
                    "role": "assistant",
                    "tool_calls": tool_calls
                        .iter()
                        .map(|call| json!({ "id": &call.id }))
                        .collect::<Vec<_>>()
                })
                .to_string(),
            ),
            tool_calls: tool_calls.into(),
            ..LlmResponse::default()
        }
    }
}

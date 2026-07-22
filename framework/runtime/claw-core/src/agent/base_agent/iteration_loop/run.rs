use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::{BTreeMap, HashSet, VecDeque};

use claw_api::{ChatRequest, ChatStreamEvent, ToolCall};
use claw_interface::http::StreamingHttp;
use claw_interface::{Cancel, ClawHttp, ClawTimer};
use claw_tool::{RawToolInvocation, ToolExecution, ToolExecutor, ToolInvocation, ToolSetHandle};
use claw_utils::stream::StreamPart;
use futures_lite::{future, StreamExt};
use tracing::Instrument as _;

use super::types::{IterationLoopError, IterationLoopEvent, LlmStep};
use super::{
    InflightToolCall, IterationEmitter, IterationLoop, IterationStream, PendingToolPermission,
    ToolAuthorization, ToolCallId, ToolCallIdAllocator, ToolPermission, ToolPermissionPolicy,
    ToolPermissionRequest,
};

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

struct ToolCallIdentity {
    id: ToolCallId,
    provider_id: String,
}

struct PendingToolCall<'a> {
    call: PreparedToolCall,
    permission: PendingToolPermission<'a>,
}

type ToolRunOutput = Result<(ToolCallId, ToolExecution), IterationLoopError>;
type ToolRunFuture<'a> = Pin<Box<dyn Future<Output = ToolRunOutput> + 'a>>;

struct ToolRuns<'a> {
    futures: Vec<ToolRunFuture<'a>>,
    cursor: usize,
}

struct ToolCallBatch {
    order: Vec<ToolCallIdentity>,
    results: BTreeMap<ToolCallId, ToolExecution>,
}

enum ToolBatchUpdate {
    Permission(ToolPermission),
    Execution(ToolRunOutput),
}

#[derive(Debug)]
enum ToolPhaseOutcome {
    Tools(Vec<(String, ToolExecution)>),
    Interrupted,
    Cancelled,
}

impl<'a, H, Timer, P> IterationLoop<'a, H, Timer, P>
where
    H: ClawHttp + StreamingHttp,
    Timer: ClawTimer,
    P: ToolPermissionPolicy + 'a,
{
    /// Start one iteration and return its sole output surface.
    ///
    /// Success is the stream reaching `None`; failures are emitted as one final
    /// `Err` item. Nothing is aggregated into a terminal response value.
    pub(crate) fn run(self, step: LlmStep<'a>) -> IterationStream<'a> {
        let (events, receiver) = IterationStream::channel();
        let driver_events = events.clone();
        let driver = Box::pin(async move {
            let span = tracing::info_span!("iteration_loop", run.iteration = %step.iteration_id);
            if let Err(error) = run_one_iteration(self, step, &driver_events)
                .instrument(span)
                .await
            {
                driver_events.send_error(error).await;
            }
        });
        IterationStream::new(driver, receiver)
    }
}

async fn run_one_iteration<H, Timer, P>(
    mut loop_: IterationLoop<'_, H, Timer, P>,
    step: LlmStep<'_>,
    events: &IterationEmitter,
) -> Result<(), IterationLoopError>
where
    H: ClawHttp + StreamingHttp,
    Timer: ClawTimer,
    P: ToolPermissionPolicy,
{
    let Some(tool_calls) = call_llm(&mut loop_, &step, events).await? else {
        events.send(IterationLoopEvent::Cancelled).await;
        return Ok(());
    };
    if tool_calls.is_empty() {
        return Ok(());
    }

    events
        .send(IterationLoopEvent::BeforeToolCalls(observable_tool_calls(
            &tool_calls,
        )))
        .await;
    match execute_tool_calls(
        &tool_calls,
        step.tools,
        loop_.control,
        loop_.permission,
        events,
    )
    .await?
    {
        ToolPhaseOutcome::Tools(results) => {
            for (tool_call_id, execution) in results {
                events
                    .send(IterationLoopEvent::ToolResult {
                        tool_call_id,
                        execution,
                    })
                    .await;
            }
        }
        ToolPhaseOutcome::Interrupted => events.send(IterationLoopEvent::Interrupted).await,
        ToolPhaseOutcome::Cancelled => events.send(IterationLoopEvent::Cancelled).await,
    }
    Ok(())
}

async fn call_llm<H, Timer, P>(
    loop_: &mut IterationLoop<'_, H, Timer, P>,
    step: &LlmStep<'_>,
    events: &IterationEmitter,
) -> Result<Option<Vec<ToolCall>>, IterationLoopError>
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
    let mut tool_calls = Vec::new();

    loop {
        let next = StreamExt::next(&mut stream)
            .instrument(chat_span.clone())
            .await;
        match next {
            Some(Ok(event)) => {
                if let ChatStreamEvent::ToolCalls(StreamPart::Delta(call)) = &event {
                    tool_calls.push(call.clone());
                }
                events.send(IterationLoopEvent::Llm(event)).await;
            }
            Some(Err(error)) => {
                return interpret_chat_error(loop_.control.is_cancelled(), error);
            }
            None => break,
        }
    }

    if loop_.control.is_cancelled() {
        tracing::warn!(name: "cancelled", checkpoint = "after_llm");
        Ok(None)
    } else {
        Ok(Some(tool_calls))
    }
}

async fn execute_tool_calls<P>(
    tool_calls: &[ToolCall],
    tools: &ToolSetHandle<'_>,
    control: &super::super::stream::RunControl,
    permission: &P,
    events: &IterationEmitter,
) -> Result<ToolPhaseOutcome, IterationLoopError>
where
    P: ToolPermissionPolicy,
{
    let mut batch = ToolCallBatch::new(tool_calls.len());
    let mut provider_ids = HashSet::with_capacity(tool_calls.len());
    let mut tool_call_ids = ToolCallIdAllocator::new();
    let mut calls = Vec::with_capacity(tool_calls.len());
    for tool_call in tool_calls {
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
            return Ok(ToolPhaseOutcome::Cancelled);
        }
        let invocation = match ToolInvocation::try_from(RawToolInvocation {
            id: Some(&tool_call.id),
            name: &tool_call.name,
            arguments_json: &tool_call.arguments_json,
        }) {
            Ok(invocation) => invocation,
            Err(error) => {
                batch.collect(
                    id,
                    ToolExecution {
                        content: error.to_string(),
                        ok: false,
                    },
                );
                continue;
            }
        };
        let action = match tools.classify(&invocation) {
            Ok(action) => action,
            Err(error) => {
                batch.collect(
                    id,
                    ToolExecution {
                        content: error.to_string(),
                        ok: false,
                    },
                );
                continue;
            }
        };
        let prepared = PreparedToolCall {
            id,
            provider_id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments_json: invocation.arguments_json().to_owned(),
        };

        match permission.authorize(
            ToolPermissionRequest {
                tool_call_id: id,
                tool_call: &tool_call,
                action: &action,
            },
            events,
        ) {
            ToolAuthorization::Allow => runs.push(execute_prepared_tool(&executor, prepared)),
            ToolAuthorization::Deny(reason) => batch.collect(
                id,
                ToolExecution {
                    content: reason,
                    ok: false,
                },
            ),
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
            return Ok(ToolPhaseOutcome::Cancelled);
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
                batch.collect(id, result);
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
                        batch.collect(
                            waiting.call.id,
                            ToolExecution {
                                content: reason,
                                ok: false,
                            },
                        );
                    }
                    ToolPermission::Interrupted => return Ok(ToolPhaseOutcome::Interrupted),
                    ToolPermission::Cancelled => {
                        return Ok(ToolPhaseOutcome::Cancelled);
                    }
                }
                active_permission = pending.pop_front();
            }
        }
    }

    if control.is_cancelled() {
        return Ok(ToolPhaseOutcome::Cancelled);
    }
    if !batch.is_complete() {
        return Err(IterationLoopError::IncompleteToolBatch);
    }
    Ok(ToolPhaseOutcome::Tools(batch.into_results()))
}

impl ToolCallBatch {
    fn new(capacity: usize) -> Self {
        Self {
            order: Vec::with_capacity(capacity),
            results: BTreeMap::new(),
        }
    }

    fn collect(&mut self, id: ToolCallId, execution: ToolExecution) {
        let previous = self.results.insert(id, execution);
        debug_assert!(previous.is_none());
    }

    fn is_complete(&self) -> bool {
        self.results.len() == self.order.len()
            && self
                .order
                .iter()
                .all(|call| self.results.contains_key(&call.id))
    }

    fn into_results(mut self) -> Vec<(String, ToolExecution)> {
        let mut results = Vec::with_capacity(self.order.len());
        for call in self.order {
            let Some(execution) = self.results.remove(&call.id) else {
                continue;
            };
            results.push((call.provider_id, execution));
        }
        results
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
    let execution = executor.execute(&invocation).instrument(span.clone()).await;
    span.in_scope(|| {
        if execution.ok {
            tracing::info!(name: "result", ok = true);
        } else {
            tracing::warn!(name: "result", ok = false);
        }
    });
    Ok((id, execution))
}

fn observable_tool_calls(tool_calls: &[ToolCall]) -> Vec<InflightToolCall> {
    tool_calls
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
) -> Result<Option<Vec<ToolCall>>, IterationLoopError> {
    if cancelled || error.is_aborted() {
        tracing::warn!(name: "cancelled", checkpoint = "in_llm_http_abort");
        Ok(None)
    } else {
        tracing::error!(name: "chat_failed", kind = "chat");
        Err(IterationLoopError::Chat(error))
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
        fn authorize<'a>(
            &'a self,
            request: ToolPermissionRequest<'_>,
            _events: &IterationEmitter,
        ) -> ToolAuthorization<'a> {
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
        let tool_calls = [
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
        ];
        let control = AgentStreamHandle::control();
        let (events, _receiver) = IterationStream::channel();
        let permission = SelectivePermission {
            executions: Arc::clone(&executions),
            checks: Cell::new(0),
            ids: RefCell::new(Vec::new()),
        };

        let result = block_on(execute_tool_calls(
            &tool_calls,
            &tools,
            &control,
            &permission,
            &events,
        ))
        .expect("tool calls complete");
        let ToolPhaseOutcome::Tools(results) = result else {
            panic!("iteration should produce tool messages");
        };

        assert_eq!(permission.checks.get(), 2);
        assert_eq!(
            permission.ids.into_inner(),
            [ToolCallId::new(0), ToolCallId::new(1)]
        );
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(
            results,
            vec![
                (
                    "call-deny".to_owned(),
                    ToolExecution {
                        content: "policy denied".to_owned(),
                        ok: false,
                    },
                ),
                (
                    "call-allow".to_owned(),
                    ToolExecution {
                        content: "allowed".to_owned(),
                        ok: true,
                    },
                ),
            ]
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
        let tool_calls = [
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
        ];
        let control = AgentStreamHandle::control();
        let permission = AllowAll;
        let (events, _receiver) = IterationStream::channel();

        let error = block_on(execute_tool_calls(
            &tool_calls,
            &tools,
            &control,
            &permission,
            &events,
        ))
        .expect_err("duplicate ids fail the iteration");

        assert_eq!(
            error,
            IterationLoopError::DuplicateProviderToolCallId("duplicate".to_owned())
        );
        assert_eq!(executions.load(Ordering::SeqCst), 0);
    }
}

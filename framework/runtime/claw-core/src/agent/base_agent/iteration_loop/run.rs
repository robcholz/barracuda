use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::{HashSet, VecDeque};

use claw_api::{ChatRequest, ChatStreamEvent, ToolCall};
use claw_interface::http::StreamingHttp;
use claw_interface::{Cancel, ClawHttp, ClawTimer};
use claw_tool::{ToolExecution, ToolExecutor, ToolInvocation, ToolSetHandle};
use claw_utils::stream::StreamPart;
use futures_lite::{future, StreamExt};
use tracing::Instrument as _;

use super::types::{IterationEvent, IterationLoopError, IterationLoopEvent, LlmStep};
use super::{
    IterationLoop, PendingToolPermission, PermissionActivation, ToolAuthorization, ToolCallId,
    ToolCallIdAllocator, ToolPermission, ToolPermissionPolicy, ToolPermissionRequest,
};

struct ScheduledCall {
    id: ToolCallId,
    call: ToolCall,
}

impl ScheduledCall {
    fn approval_call(&self) -> ToolCall {
        self.call.clone()
    }
}

struct PendingApproval<'a> {
    call: ScheduledCall,
    reason: Option<String>,
    activate: Option<PermissionActivation<'a>>,
    permission: PendingToolPermission<'a>,
    announced: bool,
}

type ToolRunOutput = Result<(ToolCall, ToolExecution), IterationLoopError>;
type ToolRunFuture<'a> = Pin<Box<dyn Future<Output = ToolRunOutput> + 'a>>;

struct ToolRuns<'a> {
    futures: Vec<ToolRunFuture<'a>>,
    cursor: usize,
}

enum ToolBatchUpdate {
    Permission(ToolPermission),
    Execution(ToolRunOutput),
}

struct ToolPhase<'a> {
    tools: &'a ToolSetHandle<'a>,
    runs: ToolRuns<'a>,
    pending: VecDeque<PendingApproval<'a>>,
    active_permission: Option<PendingApproval<'a>>,
    ready_results: VecDeque<(ToolCall, ToolExecution)>,
    remaining_results: usize,
    results_ended: bool,
}

impl<'a, H, Timer, P> IterationLoop<'a, H, Timer, P>
where
    H: ClawHttp + StreamingHttp,
    Timer: ClawTimer,
    P: ToolPermissionPolicy + 'a,
{
    /// Run one LLM/tool iteration as a directly polled stream.
    ///
    /// A successful iteration ends at `None`; failures are yielded as `Err`.
    /// Code after each `yield` cannot run until the owner polls again, making
    /// `BeforeToolCalls` a natural pre-execution boundary.
    pub(crate) fn run(
        self,
        step: LlmStep<'a>,
    ) -> impl futures_core::Stream<Item = Result<IterationLoopEvent, IterationLoopError>> + 'a {
        async_stream::try_stream! {
            let loop_ = self;
            if loop_.control.is_cancelled() {
                tracing::warn!(name: "cancelled", checkpoint = "before_llm_http");
                yield IterationLoopEvent::Cancelled;
                return;
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
            let chat_span = tracing::info_span!(
                "api.chat",
                purpose = "iteration",
                max_attempts,
                run.iteration = %step.iteration_id,
            );
            let stream_result = loop_
                .llm
                .chat_stream(&chat_request, cancel)
                .instrument(chat_span.clone())
                .await;
            let mut stream = match stream_result {
                Ok(stream) => stream,
                Err(error) if loop_.control.is_cancelled() || error.is_aborted() => {
                    tracing::warn!(name: "cancelled", checkpoint = "in_llm_http_abort");
                    yield IterationLoopEvent::Cancelled;
                    return;
                }
                Err(error) => {
                    tracing::error!(name: "chat_failed", kind = "chat");
                    Err(IterationLoopError::Chat(error))?
                }
            };

            let mut tool_calls = Vec::new();
            loop {
                let next = StreamExt::next(&mut stream)
                    .instrument(chat_span.clone())
                    .await;
                match next {
                    Some(Ok(ChatStreamEvent::Reasoning(part))) => {
                        yield IterationLoopEvent::Iteration(IterationEvent::Reasoning(part));
                    }
                    Some(Ok(ChatStreamEvent::Output(part))) => {
                        yield IterationLoopEvent::Iteration(IterationEvent::Output(part));
                    }
                    Some(Ok(ChatStreamEvent::ToolCalls(StreamPart::Delta(call)))) => {
                        tool_calls.push(call);
                    }
                    Some(Ok(ChatStreamEvent::ToolCalls(StreamPart::End))) => {}
                    Some(Err(error)) if loop_.control.is_cancelled() || error.is_aborted() => {
                        tracing::warn!(name: "cancelled", checkpoint = "in_llm_http_abort");
                        yield IterationLoopEvent::Cancelled;
                        return;
                    }
                    Some(Err(error)) => {
                        tracing::error!(name: "chat_failed", kind = "chat");
                        Err(IterationLoopError::Chat(error))?;
                    }
                    None => break,
                }
            }

            if loop_.control.is_cancelled() {
                tracing::warn!(name: "cancelled", checkpoint = "after_llm");
                yield IterationLoopEvent::Cancelled;
                return;
            }
            if tool_calls.is_empty() {
                yield IterationLoopEvent::Iteration(IterationEvent::ToolResult(StreamPart::End));
                return;
            }

            yield IterationLoopEvent::Iteration(IterationEvent::BeforeToolCalls(
                tool_calls.clone(),
            ));

            let mut tools = ToolPhase::new(tool_calls, step.tools, loop_.permission)?;
            while let Some(event) = tools.next(loop_.control).await? {
                let terminal = matches!(
                    event,
                    IterationLoopEvent::Interrupted | IterationLoopEvent::Cancelled
                );
                yield event;
                if terminal {
                    return;
                }
            }
        }
    }
}

impl<'a> ToolPhase<'a> {
    fn new<P>(
        tool_calls: Vec<ToolCall>,
        tools: &'a ToolSetHandle<'a>,
        permission: &'a P,
    ) -> Result<Self, IterationLoopError>
    where
        P: ToolPermissionPolicy,
    {
        let mut provider_ids = HashSet::with_capacity(tool_calls.len());
        for tool_call in &tool_calls {
            if tool_call.id.is_empty() {
                return Err(IterationLoopError::MissingProviderToolCallId);
            }
            if !provider_ids.insert(tool_call.id.as_str()) {
                return Err(IterationLoopError::DuplicateProviderToolCallId(
                    tool_call.id.clone(),
                ));
            }
        }

        let remaining_results = tool_calls.len();
        let mut tool_call_ids = ToolCallIdAllocator::new();
        let mut runs = ToolRuns::new();
        let mut pending = VecDeque::new();
        let mut ready_results = VecDeque::new();

        for tool_call in tool_calls {
            let id = tool_call_ids.next();
            let invocation = match ToolInvocation::try_new(
                Some(&tool_call.id),
                &tool_call.name,
                &tool_call.arguments_json,
            ) {
                Ok(invocation) => invocation,
                Err(error) => {
                    ready_results.push_back((
                        tool_call,
                        ToolExecution {
                            content: error.to_string(),
                            ok: false,
                        },
                    ));
                    continue;
                }
            };
            let action = match tools.classify(&invocation) {
                Ok(action) => action,
                Err(error) => {
                    ready_results.push_back((
                        tool_call,
                        ToolExecution {
                            content: error.to_string(),
                            ok: false,
                        },
                    ));
                    continue;
                }
            };
            let scheduled = ScheduledCall {
                id,
                call: tool_call,
            };

            match permission.authorize(ToolPermissionRequest {
                tool_call_id: id,
                action: &action,
            }) {
                ToolAuthorization::Allow => {
                    runs.push(execute_scheduled_call(tools, scheduled));
                }
                ToolAuthorization::Deny(reason) => ready_results.push_back((
                    scheduled.call,
                    ToolExecution {
                        content: reason,
                        ok: false,
                    },
                )),
                ToolAuthorization::Pending {
                    reason,
                    activate,
                    permission,
                } => pending.push_back(PendingApproval {
                    call: scheduled,
                    reason: Some(reason),
                    activate: Some(activate),
                    permission,
                    announced: false,
                }),
            }
        }

        Ok(Self {
            tools,
            runs,
            pending,
            active_permission: None,
            ready_results,
            remaining_results,
            results_ended: false,
        })
    }

    async fn next(
        &mut self,
        control: &super::super::stream::RunControl,
    ) -> Result<Option<IterationLoopEvent>, IterationLoopError> {
        loop {
            if let Some((call, execution)) = self.ready_results.pop_front() {
                return self.tool_result(call, execution).map(Some);
            }
            if self.results_ended {
                return Ok(None);
            }
            if self.remaining_results == 0 {
                self.results_ended = true;
                return Ok(Some(IterationLoopEvent::Iteration(
                    IterationEvent::ToolResult(StreamPart::End),
                )));
            }
            if control.is_cancelled() {
                self.results_ended = true;
                return Ok(Some(IterationLoopEvent::Cancelled));
            }

            if self.active_permission.is_none() {
                self.active_permission = self.pending.pop_front();
            }
            if let Some(waiting) = self.active_permission.as_mut() {
                if !waiting.announced {
                    if let Some(activate) = waiting.activate.take() {
                        activate();
                    }
                    waiting.announced = true;
                    return Ok(Some(IterationLoopEvent::ApprovalRequired {
                        tool_call_id: waiting.call.id,
                        tool_call: waiting.call.approval_call(),
                        reason: waiting.reason.take().unwrap_or_default(),
                    }));
                }
            }

            if self.active_permission.is_none() && self.runs.is_empty() {
                return Err(IterationLoopError::IncompleteToolBatch);
            }

            let update = match self.active_permission.as_mut() {
                Some(waiting) if !self.runs.is_empty() => {
                    future::or(
                        async { ToolBatchUpdate::Permission(waiting.permission.as_mut().await) },
                        async { ToolBatchUpdate::Execution(self.runs.next().await) },
                    )
                    .await
                }
                Some(waiting) => ToolBatchUpdate::Permission(waiting.permission.as_mut().await),
                None => ToolBatchUpdate::Execution(self.runs.next().await),
            };

            match update {
                ToolBatchUpdate::Execution(result) => {
                    let (call, execution) = result?;
                    return self.tool_result(call, execution).map(Some);
                }
                ToolBatchUpdate::Permission(decision) => {
                    let waiting = self
                        .active_permission
                        .take()
                        .ok_or(IterationLoopError::IncompleteToolBatch)?;
                    match decision {
                        ToolPermission::Allow => {
                            self.runs
                                .push(execute_scheduled_call(self.tools, waiting.call));
                        }
                        ToolPermission::Deny(reason) => {
                            return self
                                .tool_result(
                                    waiting.call.call,
                                    ToolExecution {
                                        content: reason,
                                        ok: false,
                                    },
                                )
                                .map(Some);
                        }
                        ToolPermission::Interrupted => {
                            self.results_ended = true;
                            return Ok(Some(IterationLoopEvent::Interrupted));
                        }
                        ToolPermission::Cancelled => {
                            self.results_ended = true;
                            return Ok(Some(IterationLoopEvent::Cancelled));
                        }
                    }
                }
            }
        }
    }

    fn tool_result(
        &mut self,
        call: ToolCall,
        execution: ToolExecution,
    ) -> Result<IterationLoopEvent, IterationLoopError> {
        self.remaining_results = self
            .remaining_results
            .checked_sub(1)
            .ok_or(IterationLoopError::IncompleteToolBatch)?;
        Ok(IterationLoopEvent::Iteration(IterationEvent::ToolResult(
            StreamPart::Delta((call, execution)),
        )))
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

async fn execute_scheduled_call(tools: &ToolSetHandle<'_>, call: ScheduledCall) -> ToolRunOutput {
    let span = tracing::info_span!("toolcall", tool = %call.call.name);
    let execution = {
        let invocation = match ToolInvocation::try_new(
            Some(&call.call.id),
            &call.call.name,
            &call.call.arguments_json,
        ) {
            Ok(invocation) => invocation,
            Err(error) => {
                return Ok((
                    call.call,
                    ToolExecution {
                        content: error.to_string(),
                        ok: false,
                    },
                ));
            }
        };
        ToolExecutor::new(tools)
            .execute(&invocation)
            .instrument(span.clone())
            .await
    };
    span.in_scope(|| {
        if execution.ok {
            tracing::info!(name: "result", ok = true);
        } else {
            tracing::warn!(name: "result", ok = false);
        }
    });
    Ok((call.call, execution))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use claw_permission::{AllowAll, RiskClass};
    use claw_tool::{
        SyncToolHandler, Tool, ToolGroup, ToolInvocation, ToolOutput, ToolRegistry, ToolResult,
        ToolSpec,
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
            r#"{"type":"function","function":{"name":"test","parameters":{"type":"object"}}}"#
        }

        fn classify(&self, _call: &ToolInvocation<'_>) -> claw_permission::Action {
            claw_permission::Action::new(self.name, RiskClass::High)
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
            assert_eq!(self.executions.load(Ordering::SeqCst), 0);
            self.checks.set(self.checks.get() + 1);
            self.ids.borrow_mut().push(request.tool_call_id);
            if request.action.verb() == "denied" {
                let executions = Arc::clone(&self.executions);
                ToolAuthorization::Pending {
                    reason: "policy check".to_owned(),
                    activate: Box::new(|| {}),
                    permission: Box::pin(async move {
                        while executions.load(Ordering::SeqCst) == 0 {
                            futures_lite::future::yield_now().await;
                        }
                        ToolPermission::Deny("policy denied".to_owned())
                    }),
                }
            } else {
                ToolAuthorization::Allow
            }
        }
    }

    fn test_tools(executions: &Arc<AtomicUsize>) -> (Arc<ToolRegistry>, claw_tool::ToolSet) {
        let registry = Arc::new(ToolRegistry::new());
        let mut tool_set = registry.tool_set();
        tool_set
            .add_group(ToolGroup::new(
                "test",
                true,
                [
                    Tool::from_sync(CountingTool {
                        name: "allowed",
                        calls: Arc::clone(executions),
                    }),
                    Tool::from_sync(CountingTool {
                        name: "denied",
                        calls: Arc::clone(executions),
                    }),
                ],
            ))
            .expect("test tools are valid");
        (registry, tool_set)
    }

    #[test]
    fn allowed_tools_run_while_permission_is_pending_and_results_use_completion_order() {
        let executions = Arc::new(AtomicUsize::new(0));
        let (_registry, mut tool_set) = test_tools(&executions);
        let tools = tool_set.begin().expect("test tool set starts");
        let tool_calls = vec![
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
        let permission = SelectivePermission {
            executions: Arc::clone(&executions),
            checks: Cell::new(0),
            ids: RefCell::new(Vec::new()),
        };
        let mut phase = ToolPhase::new(tool_calls, &tools, &permission).expect("phase prepares");

        let events = block_on(async {
            let mut events = Vec::new();
            while let Some(event) = phase.next(&control).await.expect("tool phase advances") {
                events.push(event);
            }
            events
        });

        assert!(matches!(
            events.first(),
            Some(IterationLoopEvent::ApprovalRequired { tool_call_id, .. })
                if *tool_call_id == ToolCallId::new(0)
        ));
        assert_eq!(permission.checks.get(), 2);
        assert_eq!(
            permission.ids.borrow().as_slice(),
            [ToolCallId::new(0), ToolCallId::new(1)]
        );
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert!(matches!(
            events.last(),
            Some(IterationLoopEvent::Iteration(IterationEvent::ToolResult(
                StreamPart::End
            )))
        ));
        let results = events
            .into_iter()
            .filter_map(|event| match event {
                IterationLoopEvent::Iteration(IterationEvent::ToolResult(StreamPart::Delta((
                    call,
                    execution,
                )))) => Some((call.id, execution)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            results,
            vec![
                (
                    "call-allow".to_owned(),
                    ToolExecution {
                        content: "allowed".to_owned(),
                        ok: true,
                    },
                ),
                (
                    "call-deny".to_owned(),
                    ToolExecution {
                        content: "policy denied".to_owned(),
                        ok: false,
                    },
                ),
            ]
        );
    }

    #[test]
    fn yolo_policy_rejects_duplicate_provider_ids_before_execution() {
        let executions = Arc::new(AtomicUsize::new(0));
        let (_registry, mut tool_set) = test_tools(&executions);
        let tools = tool_set.begin().expect("test tool set starts");
        let tool_calls = vec![
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

        let error = ToolPhase::new(tool_calls, &tools, &AllowAll)
            .err()
            .expect("duplicate ids fail the iteration");
        assert_eq!(
            error,
            IterationLoopError::DuplicateProviderToolCallId("duplicate".to_owned())
        );
        assert_eq!(executions.load(Ordering::SeqCst), 0);
    }
}

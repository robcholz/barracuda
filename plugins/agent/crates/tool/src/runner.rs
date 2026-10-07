use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_runtime_utils::oneshot;
use barracuda_runtime_utils::unordered::{Merged, Unordered};
use futures_core::Stream;
use portable_atomic::{AtomicU32, Ordering};
use tracing::Instrument as _;

use super::{
    Tool, ToolCompletionFuture, ToolDetachUpdate, ToolInvocation, ToolOutput, ToolResult,
    ToolSetHandle,
};
use crate::definition::ToolProgressReceiver;

const DETACHED_ACCEPTED: &str = concat!(
    "[detached:accepted]\n",
    "The tool is running in the background. ",
    "Its result will be delivered automatically."
);

type ToolRunFuture = Pin<Box<dyn Future<Output = Option<(ToolInvocation, ToolOutput)>> + 'static>>;
type ToolDetachStream = Pin<Box<dyn Stream<Item = (ToolInvocation, ToolDetachUpdate)> + 'static>>;
type DetachedExecution = (Option<ToolProgressReceiver>, ToolCompletionFuture);

static NEXT_TOOL_TASK_ID: AtomicU32 = AtomicU32::new(0);

#[derive(Default)]
struct ToolRuns {
    runs: Unordered<ToolRunFuture>,
}

#[derive(Default)]
struct ToolDetachRuns {
    runs: Merged<ToolDetachStream>,
}

impl ToolDetachRuns {
    fn push(&mut self, stream: ToolDetachStream) {
        self.runs.push(stream);
    }

    fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    fn poll_next(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Option<(ToolInvocation, ToolDetachUpdate)>> {
        Pin::new(&mut self.runs).poll_next(context)
    }
}

impl ToolRuns {
    fn push(&mut self, future: ToolRunFuture) {
        self.runs.push(future);
    }

    fn merge(&mut self, other: Self) {
        self.runs.extend(other.runs);
    }

    fn poll_next(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Option<(ToolInvocation, ToolOutput)>> {
        // A settled run yields `Some(result)`; a run that finished without a
        // model-facing result yields `None` and is simply drained.
        loop {
            match Pin::new(&mut self.runs).poll_next(context) {
                Poll::Ready(Some(Some(result))) => return Poll::Ready(Some(result)),
                Poll::Ready(Some(None)) => continue,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Stream of model-facing settlements for one dispatched tool batch.
///
/// Joined calls produce their real result. Detached calls produce their
/// immediate accepted result.
pub struct ToolJoinHandle {
    runs: ToolRuns,
}

impl ToolJoinHandle {
    pub fn merge(&mut self, other: Self) {
        self.runs.merge(other.runs);
    }
}

impl Stream for ToolJoinHandle {
    type Item = (ToolInvocation, ToolOutput);

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.runs.poll_next(context)
    }
}

/// Stream of progress and implicit completion updates for detached calls.
pub struct ToolDetachHandle {
    runs: ToolDetachRuns,
}

impl Stream for ToolDetachHandle {
    type Item = (ToolInvocation, ToolDetachUpdate);

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.runs.poll_next(context)
    }
}

/// Dispatches one authorized tool batch without polling any invocation.
pub struct ToolRunner<'a> {
    tools: &'a ToolSetHandle<'a>,
}

impl<'a> ToolRunner<'a> {
    pub fn new(tools: &'a ToolSetHandle<'a>) -> Self {
        Self { tools }
    }

    pub fn run(&self, calls: Vec<ToolInvocation>) -> (ToolJoinHandle, Option<ToolDetachHandle>) {
        let mut joined = ToolRuns::default();
        let mut detached = ToolDetachRuns::default();

        for invocation in calls {
            let span = toolcall_span(&invocation);
            let tool = match self.tools.runnable_tool(&invocation) {
                Ok(tool) => tool,
                Err(error) => {
                    joined.push(traced_ready(invocation, Err(error), span, true));
                    continue;
                }
            };
            if tool.is_dynamically_detached() {
                let (completion, receiver) = oneshot::channel();
                joined.push(start_detached(
                    tool,
                    invocation.clone(),
                    completion,
                    span.clone(),
                ));
                detached.push(dynamic_detached_updates(invocation, receiver, span));
            } else if tool.config().detached {
                joined.push(ready(invocation.clone(), Ok(detached_accepted())));
                detached.push(static_detached_update(tool, invocation, span));
            } else {
                joined.push(run(tool, invocation, span));
            }
        }

        let detached = (!detached.is_empty()).then_some(ToolDetachHandle { runs: detached });
        (ToolJoinHandle { runs: joined }, detached)
    }
}

fn ready(invocation: ToolInvocation, output: ToolResult<ToolOutput>) -> ToolRunFuture {
    Box::pin(async move { Some((invocation, settle(output))) })
}

fn traced_ready(
    invocation: ToolInvocation,
    output: ToolResult<ToolOutput>,
    span: tracing::Span,
    blocked: bool,
) -> ToolRunFuture {
    Box::pin(
        async move {
            let output = settle(output);
            trace_result(&output, blocked);
            Some((invocation, output))
        }
        .instrument(span),
    )
}

fn run(tool: Tool, invocation: ToolInvocation, span: tracing::Span) -> ToolRunFuture {
    Box::pin(
        async move {
            let output = tool.invoke(&invocation).await;
            let output = settle(output);
            trace_result(&output, false);
            Some((invocation, output))
        }
        .instrument(span),
    )
}

fn start_detached(
    tool: Tool,
    invocation: ToolInvocation,
    completion: oneshot::Sender<DetachedExecution>,
    span: tracing::Span,
) -> ToolRunFuture {
    Box::pin(
        async move {
            let output = match tool.invoke_detached(&invocation).await {
                Ok(detached) => {
                    let (accepted, progress, future) = detached.into_parts();
                    let _ = completion.send((progress, future));
                    accepted
                }
                Err(error) => {
                    let output = settle(Err(error));
                    trace_result(&output, false);
                    output
                }
            };
            Some((invocation, output))
        }
        .instrument(span),
    )
}

fn static_detached_update(
    tool: Tool,
    invocation: ToolInvocation,
    span: tracing::Span,
) -> ToolDetachStream {
    Box::pin(futures_util::stream::once(
        async move {
            let output = settle(tool.invoke(&invocation).await);
            trace_result(&output, false);
            (invocation, ToolDetachUpdate::Completed(output))
        }
        .instrument(span),
    ))
}

fn dynamic_detached_updates(
    invocation: ToolInvocation,
    execution: oneshot::Receiver<DetachedExecution>,
    span: tracing::Span,
) -> ToolDetachStream {
    Box::pin(DynamicDetachedUpdates {
        invocation,
        state: DynamicDetachedState::Starting(execution),
        span,
    })
}

enum DynamicDetachedState {
    Starting(oneshot::Receiver<DetachedExecution>),
    Running {
        progress: Option<ToolProgressReceiver>,
        completion: Option<ToolCompletionFuture>,
        terminal: Option<ToolOutput>,
    },
    Done,
}

struct DynamicDetachedUpdates {
    invocation: ToolInvocation,
    state: DynamicDetachedState,
    span: tracing::Span,
}

impl Stream for DynamicDetachedUpdates {
    type Item = (ToolInvocation, ToolDetachUpdate);

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match &mut self.state {
                DynamicDetachedState::Starting(receiver) => {
                    match Pin::new(receiver).poll(context) {
                        Poll::Ready(Ok((progress, completion))) => {
                            self.state = DynamicDetachedState::Running {
                                progress,
                                completion: Some(completion),
                                terminal: None,
                            };
                        }
                        Poll::Ready(Err(_error)) => {
                            self.state = DynamicDetachedState::Done;
                            return Poll::Ready(None);
                        }
                        Poll::Pending => return Poll::Pending,
                    }
                }
                DynamicDetachedState::Running {
                    progress,
                    completion,
                    terminal,
                } => {
                    if let Some(receiver) = progress {
                        match Pin::new(receiver).poll_next(context) {
                            Poll::Ready(Some(output)) => {
                                return Poll::Ready(Some((
                                    self.invocation.clone(),
                                    ToolDetachUpdate::Progress(output),
                                )));
                            }
                            Poll::Ready(None) => *progress = None,
                            Poll::Pending => {}
                        }
                    }

                    if let Some(future) = completion {
                        if let Poll::Ready(output) = future.as_mut().poll(context) {
                            let output = settle(output);
                            trace_result(&output, false);
                            *completion = None;
                            *terminal = Some(output);

                            // Poll progress again because completing the handler
                            // may have synchronously published its final update.
                            if let Some(receiver) = progress {
                                match Pin::new(receiver).poll_next(context) {
                                    Poll::Ready(Some(output)) => {
                                        return Poll::Ready(Some((
                                            self.invocation.clone(),
                                            ToolDetachUpdate::Progress(output),
                                        )));
                                    }
                                    Poll::Ready(None) => *progress = None,
                                    Poll::Pending => {}
                                }
                            }
                        }
                    }

                    if let Some(output) = terminal.take() {
                        let invocation = self.invocation.clone();
                        self.span.in_scope(|| {
                            tracing::debug!("detached tool completed");
                        });
                        self.state = DynamicDetachedState::Done;
                        return Poll::Ready(Some((
                            invocation,
                            ToolDetachUpdate::Completed(output),
                        )));
                    }
                    return Poll::Pending;
                }
                DynamicDetachedState::Done => return Poll::Ready(None),
            }
        }
    }
}

fn toolcall_span(invocation: &ToolInvocation) -> tracing::Span {
    let task_id = NEXT_TOOL_TASK_ID.fetch_add(1, Ordering::Relaxed);
    let task = format!("toolcall-{task_id}");
    let span = tracing::info_span!(
        "toolcall",
        trace.task = %task,
        tool = %invocation.name(),
    );
    span.in_scope(|| {
        tracing::info!(
            name: "arguments",
            argument_bytes = invocation.arguments_json().len() as u64,
        );
    });
    span
}

fn trace_result(output: &ToolOutput, blocked: bool) {
    if output.ok {
        tracing::info!(name: "result", ok = output.ok, blocked);
    } else {
        tracing::warn!(name: "result", ok = output.ok, blocked);
    }
}

fn detached_accepted() -> ToolOutput {
    ToolOutput {
        content: DETACHED_ACCEPTED.to_owned(),
        ok: true,
    }
}

fn settle(output: ToolResult<ToolOutput>) -> ToolOutput {
    match output {
        Ok(output) => output,
        Err(error) => ToolOutput {
            content: error.to_string(),
            ok: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;

    use super::*;
    use crate::{
        DetachedTool, DetachedToolFuture, DetachedToolHandler, EmptyArgs, ToolConfig,
        ToolDetachUpdate, ToolFuture, ToolGroup, ToolHandler, ToolSet, ToolSpec,
    };

    struct EchoTool {
        name: &'static str,
    }

    impl ToolSpec for EchoTool {
        fn name(&self) -> &str {
            self.name
        }

        fn schema(&self) -> &str {
            r#"{"type":"function","function":{"name":"echo","parameters":{"type":"object"}}}"#
        }

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator =
                json_validator::validator!("tests/fixtures/object.json");
            &VALIDATOR
        }
    }

    impl ToolHandler for EchoTool {
        type Args = EmptyArgs;

        fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
            Box::pin(async move {
                Ok(ToolOutput {
                    content: self.name.to_owned(),
                    ok: true,
                })
            })
        }
    }

    struct DynamicDetachedTool;

    impl ToolSpec for DynamicDetachedTool {
        fn name(&self) -> &str {
            "dynamic"
        }

        fn schema(&self) -> &str {
            r#"{"type":"function","function":{"name":"dynamic","parameters":{"type":"object"}}}"#
        }

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator =
                json_validator::validator!("tests/fixtures/object.json");
            &VALIDATOR
        }
    }

    struct ProgressingDetachedTool;

    impl ToolSpec for ProgressingDetachedTool {
        fn name(&self) -> &str {
            "progressing"
        }

        fn schema(&self) -> &str {
            r#"{"type":"function","function":{"name":"progressing","parameters":{"type":"object"}}}"#
        }

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator =
                json_validator::validator!("tests/fixtures/object.json");
            &VALIDATOR
        }
    }

    impl DetachedToolHandler for ProgressingDetachedTool {
        type Args = EmptyArgs;

        fn invoke<'a>(&'a self, _args: Self::Args) -> DetachedToolFuture<'a> {
            Box::pin(async {
                Ok(DetachedTool::with_progress(
                    ToolOutput {
                        content: "accepted-progressing".to_owned(),
                        ok: true,
                    },
                    |progress| {
                        Box::pin(async move {
                            progress.send(ToolOutput {
                                content: "input-required".to_owned(),
                                ok: true,
                            });
                            Ok(ToolOutput {
                                content: "completed-progressing".to_owned(),
                                ok: true,
                            })
                        })
                    },
                ))
            })
        }
    }

    impl DetachedToolHandler for DynamicDetachedTool {
        type Args = EmptyArgs;

        fn invoke<'a>(&'a self, _args: Self::Args) -> DetachedToolFuture<'a> {
            Box::pin(async {
                Ok(DetachedTool::new(
                    ToolOutput {
                        content: "accepted-with-id".to_owned(),
                        ok: true,
                    },
                    Box::pin(async {
                        Ok(ToolOutput {
                            content: "completed-later".to_owned(),
                            ok: true,
                        })
                    }),
                ))
            })
        }
    }

    #[test]
    fn run_splits_one_batch_into_join_and_detach_streams() {
        let mut tools = ToolSet::empty();
        let added = tools.add_group(ToolGroup::new(
            "test",
            true,
            [
                Tool::new(EchoTool { name: "joined" }),
                Tool::new(EchoTool { name: "detached_a" })
                    .with_config(ToolConfig { detached: true }),
                Tool::new(EchoTool { name: "detached_b" })
                    .with_config(ToolConfig { detached: true }),
            ],
        ));
        assert!(added.is_ok());

        let started = tools.begin();
        assert!(started.is_ok());
        let Ok(tools) = started else {
            return;
        };
        let joined = ToolInvocation::try_new(Some("call-1"), "joined", "{}");
        let detached_a = ToolInvocation::try_new(Some("call-2"), "detached_a", "{}");
        let detached_b = ToolInvocation::try_new(Some("call-3"), "detached_b", "{}");
        assert!(joined.is_ok());
        assert!(detached_a.is_ok());
        assert!(detached_b.is_ok());
        let (Ok(joined), Ok(detached_a), Ok(detached_b)) = (joined, detached_a, detached_b) else {
            return;
        };

        let (join, detach) = ToolRunner::new(&tools).run(vec![joined, detached_a, detached_b]);
        let joined = block_on(join.collect::<Vec<_>>());
        assert_eq!(joined.len(), 3);
        assert!(joined.iter().any(|(invocation, output)| {
            invocation.id() == Some("call-1") && output.content == "joined"
        }));
        assert!(joined.iter().any(|(invocation, output)| {
            invocation.id() == Some("call-2") && output.content.starts_with("[detached:accepted]")
        }));
        assert!(joined.iter().any(|(invocation, output)| {
            invocation.id() == Some("call-3") && output.content.starts_with("[detached:accepted]")
        }));

        assert!(detach.is_some());
        let Some(detach) = detach else {
            return;
        };
        let detached = block_on(detach.collect::<Vec<_>>());
        assert_eq!(detached.len(), 2);
        assert!(detached.iter().any(|(invocation, update)| {
            invocation.id() == Some("call-2")
                && matches!(
                    update,
                    ToolDetachUpdate::Completed(output)
                        if output.content == "detached_a" && output.ok
                )
        }));
        assert!(detached.iter().any(|(invocation, update)| {
            invocation.id() == Some("call-3")
                && matches!(
                    update,
                    ToolDetachUpdate::Completed(output)
                        if output.content == "detached_b" && output.ok
                )
        }));
    }

    #[test]
    fn dynamic_detached_tool_controls_accepted_and_completed_outputs() {
        let mut tools = ToolSet::empty();
        assert!(tools
            .add_group(ToolGroup::new(
                "test",
                true,
                [Tool::from_detached(DynamicDetachedTool)],
            ))
            .is_ok());
        let Ok(tools) = tools.begin() else {
            return;
        };
        let Ok(call) = ToolInvocation::try_new(Some("call-dynamic"), "dynamic", "{}") else {
            return;
        };

        let (join, detach) = ToolRunner::new(&tools).run(vec![call]);
        let joined = block_on(join.collect::<Vec<_>>());
        let Some((_, accepted)) = joined.first() else {
            return;
        };
        assert_eq!(accepted.content, "accepted-with-id");

        let Some(detach) = detach else {
            return;
        };
        let completed = block_on(detach.collect::<Vec<_>>());
        let Some((_, ToolDetachUpdate::Completed(completed))) = completed.first() else {
            return;
        };
        assert_eq!(completed.content, "completed-later");
    }

    #[test]
    fn dynamic_detached_tool_reports_progress_before_implicit_completion() {
        let mut tools = ToolSet::empty();
        assert!(tools
            .add_group(ToolGroup::new(
                "test",
                true,
                [Tool::from_detached(ProgressingDetachedTool)],
            ))
            .is_ok());
        let Ok(tools) = tools.begin() else {
            return;
        };
        let Ok(call) = ToolInvocation::try_new(Some("call-progressing"), "progressing", "{}")
        else {
            return;
        };

        let (join, detach) = ToolRunner::new(&tools).run(vec![call]);
        let joined = block_on(join.collect::<Vec<_>>());
        let Some((_, accepted)) = joined.first() else {
            return;
        };
        assert_eq!(accepted.content, "accepted-progressing");

        let Some(detach) = detach else {
            return;
        };
        let updates = block_on(detach.collect::<Vec<_>>());
        assert_eq!(updates.len(), 2);
        let mut updates = updates.iter();
        let Some((_, progress)) = updates.next() else {
            return;
        };
        assert!(matches!(
            progress,
            ToolDetachUpdate::Progress(output) if output.content == "input-required"
        ));
        let Some((_, completed)) = updates.next() else {
            return;
        };
        assert!(matches!(
            completed,
            ToolDetachUpdate::Completed(output) if output.content == "completed-progressing"
        ));
    }
}

use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU32, Ordering};
use core::task::{Context, Poll};

use async_channel::{Receiver, Sender};
use futures_core::Stream;
use futures_util::stream::FuturesUnordered;
use tracing::Instrument as _;

use super::{Tool, ToolCompletionFuture, ToolInvocation, ToolOutput, ToolResult, ToolSetHandle};

const DETACHED_ACCEPTED: &str = concat!(
    "[detached:accepted]\n",
    "The tool is running in the background. ",
    "Its result will be delivered automatically."
);

type ToolRunFuture =
    Pin<Box<dyn Future<Output = Option<(ToolInvocation, ToolOutput)>> + Send + 'static>>;

static NEXT_TOOL_TASK_ID: AtomicU32 = AtomicU32::new(0);

#[derive(Default)]
struct ToolRuns {
    runs: FuturesUnordered<ToolRunFuture>,
}

impl ToolRuns {
    fn push(&mut self, future: ToolRunFuture) {
        self.runs.push(future);
    }

    fn merge(&mut self, other: Self) {
        self.runs.extend(other.runs);
    }

    fn is_empty(&self) -> bool {
        self.runs.is_empty()
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

/// Stream of real completions for all detached calls in one dispatched batch.
pub struct ToolDetachHandle {
    runs: ToolRuns,
}

impl Stream for ToolDetachHandle {
    type Item = (ToolInvocation, ToolOutput);

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
        let mut detached = ToolRuns::default();

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
                let (completion, receiver) = async_channel::bounded(1);
                joined.push(start_detached(
                    tool,
                    invocation.clone(),
                    completion,
                    span.clone(),
                ));
                detached.push(await_completion(invocation, receiver, span));
            } else if tool.config().detached {
                joined.push(ready(invocation.clone(), Ok(detached_accepted())));
                detached.push(run(tool, invocation, span));
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
    completion: Sender<ToolCompletionFuture>,
    span: tracing::Span,
) -> ToolRunFuture {
    Box::pin(
        async move {
            let output = match tool.invoke_detached(&invocation).await {
                Ok(detached) => {
                    let (accepted, future) = detached.into_parts();
                    let _ = completion.try_send(future);
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

fn await_completion(
    invocation: ToolInvocation,
    completion: Receiver<ToolCompletionFuture>,
    span: tracing::Span,
) -> ToolRunFuture {
    Box::pin(
        async move {
            let future = completion.recv().await.ok()?;
            let output = settle(future.await);
            trace_result(&output, false);
            Some((invocation, output))
        }
        .instrument(span),
    )
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
    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;

    use super::*;
    use crate::{
        DetachedTool, DetachedToolFuture, DetachedToolHandler, SyncToolHandler, ToolConfig,
        ToolGroup, ToolSet, ToolSpec,
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
    }

    impl SyncToolHandler for EchoTool {
        fn invoke(&self, _call: &ToolInvocation) -> ToolResult<ToolOutput> {
            Ok(ToolOutput {
                content: self.name.to_owned(),
                ok: true,
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
    }

    impl DetachedToolHandler for DynamicDetachedTool {
        fn invoke<'a>(&'a self, _call: &'a ToolInvocation) -> DetachedToolFuture<'a> {
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
                Tool::from_sync(EchoTool { name: "joined" }),
                Tool::from_sync(EchoTool { name: "detached_a" })
                    .with_config(ToolConfig { detached: true }),
                Tool::from_sync(EchoTool { name: "detached_b" })
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
        assert!(detached.iter().any(|(invocation, output)| {
            invocation.id() == Some("call-2") && output.content == "detached_a" && output.ok
        }));
        assert!(detached.iter().any(|(invocation, output)| {
            invocation.id() == Some("call-3") && output.content == "detached_b" && output.ok
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
        let Some((_, completed)) = completed.first() else {
            return;
        };
        assert_eq!(completed.content, "completed-later");
    }
}

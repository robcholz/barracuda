use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU32, Ordering};
use core::task::{Context, Poll};

use futures_channel::oneshot;
use futures_core::Stream;
use futures_util::stream::FuturesUnordered;
use tracing::Instrument as _;

use super::context::AgentStorageScope;
use super::{
    Tool, ToolCompletionFuture, ToolContext, ToolInvocation, ToolOutput, ToolResult, ToolSetHandle,
};

const DETACHED_ACCEPTED: &str = concat!(
    "[detached:accepted]\n",
    "The tool is running in the background. ",
    "Its result will be delivered automatically."
);

type ToolRunFuture = Pin<Box<dyn Future<Output = Option<(ToolInvocation, ToolOutput)>> + 'static>>;

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
    storage: AgentStorageScope,
}

impl<'a> ToolRunner<'a> {
    pub(crate) fn with_storage(tools: &'a ToolSetHandle<'a>, storage: AgentStorageScope) -> Self {
        Self { tools, storage }
    }

    /// Build a runner for framework-internal tool protocols that do not belong
    /// to an Agent. Storage access from such a handler fails explicitly.
    pub fn stateless(tools: &'a ToolSetHandle<'a>) -> Self {
        Self::with_storage(tools, AgentStorageScope::unavailable())
    }

    pub fn run(&self, calls: Vec<ToolInvocation>) -> (ToolJoinHandle, Option<ToolDetachHandle>) {
        let mut joined = ToolRuns::default();
        let mut detached = ToolRuns::default();

        for invocation in calls {
            let span = toolcall_span(&invocation);
            let runnable = match self.tools.runnable_tool(&invocation) {
                Ok(runnable) => runnable,
                Err(error) => {
                    joined.push(traced_ready(invocation, Err(error), span, true));
                    continue;
                }
            };
            let context = ToolContext::new(self.storage.bind(runnable.group_id));
            let tool = runnable.tool;
            if tool.is_dynamically_detached() {
                let (completion, receiver) = oneshot::channel();
                joined.push(start_detached(
                    tool,
                    context,
                    invocation.clone(),
                    completion,
                    span.clone(),
                ));
                detached.push(await_completion(invocation, receiver, span));
            } else if tool.config().detached {
                joined.push(ready(invocation.clone(), Ok(detached_accepted())));
                detached.push(run(tool, context, invocation, span));
            } else {
                joined.push(run(tool, context, invocation, span));
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

fn run(
    tool: Tool,
    context: ToolContext,
    invocation: ToolInvocation,
    span: tracing::Span,
) -> ToolRunFuture {
    Box::pin(
        async move {
            let output = tool.invoke(context, &invocation).await;
            let output = settle(output);
            trace_result(&output, false);
            Some((invocation, output))
        }
        .instrument(span),
    )
}

fn start_detached(
    tool: Tool,
    context: ToolContext,
    invocation: ToolInvocation,
    completion: oneshot::Sender<ToolCompletionFuture>,
    span: tracing::Span,
) -> ToolRunFuture {
    Box::pin(
        async move {
            let output = match tool.invoke_detached(context, &invocation).await {
                Ok(detached) => {
                    let (accepted, future) = detached.into_parts();
                    let _ = completion.send(future);
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
    completion: oneshot::Receiver<ToolCompletionFuture>,
    span: tracing::Span,
) -> ToolRunFuture {
    Box::pin(
        async move {
            let future = completion.await.ok()?;
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
    use alloc::collections::BTreeMap;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec;
    use core::cell::RefCell;
    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;

    use super::*;
    use crate::runtime::{AgentStorageBackend, AgentStorageScope};
    use crate::{
        AgentStorageError, DetachedTool, DetachedToolFuture, DetachedToolHandler, EmptyArgs,
        ToolConfig, ToolFuture, ToolGroup, ToolHandler, ToolSet, ToolSpec,
    };

    #[derive(Default)]
    struct MemoryAgentStorage {
        objects: RefCell<BTreeMap<String, serde_json::Value>>,
    }

    impl AgentStorageBackend for MemoryAgentStorage {
        fn load(&self, group: &str) -> Result<Option<serde_json::Value>, AgentStorageError> {
            Ok(self.objects.borrow().get(group).cloned())
        }

        fn store(&self, group: &str, object: serde_json::Value) -> Result<(), AgentStorageError> {
            self.objects.borrow_mut().insert(group.to_owned(), object);
            Ok(())
        }

        fn clear(&self, group: &str) -> Result<(), AgentStorageError> {
            self.objects.borrow_mut().remove(group);
            Ok(())
        }
    }

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

        fn invoke<'a>(&'a self, _context: ToolContext, _args: Self::Args) -> ToolFuture<'a> {
            Box::pin(async move {
                Ok(ToolOutput {
                    content: self.name.to_owned(),
                    ok: true,
                })
            })
        }
    }

    struct DynamicDetachedTool;

    struct StoreTool;

    impl ToolSpec for StoreTool {
        fn name(&self) -> &str {
            "store"
        }

        fn schema(&self) -> &str {
            r#"{"type":"function","function":{"name":"store","parameters":{"type":"object"}}}"#
        }

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator =
                json_validator::validator!("tests/fixtures/object.json");
            &VALIDATOR
        }
    }

    impl ToolHandler for StoreTool {
        type Args = EmptyArgs;

        fn invoke<'a>(&'a self, context: ToolContext, _args: Self::Args) -> ToolFuture<'a> {
            Box::pin(async move {
                context
                    .agent_storage
                    .store(&serde_json::json!({ "value": 7 }))?;
                Ok(ToolOutput {
                    content: "stored".to_owned(),
                    ok: true,
                })
            })
        }
    }

    struct LoadTool {
        name: &'static str,
    }

    impl ToolSpec for LoadTool {
        fn name(&self) -> &str {
            self.name
        }

        fn schema(&self) -> &str {
            r#"{"type":"function","function":{"name":"load","parameters":{"type":"object"}}}"#
        }

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator =
                json_validator::validator!("tests/fixtures/object.json");
            &VALIDATOR
        }
    }

    impl ToolHandler for LoadTool {
        type Args = EmptyArgs;

        fn invoke<'a>(&'a self, context: ToolContext, _args: Self::Args) -> ToolFuture<'a> {
            Box::pin(async move {
                let object = context
                    .agent_storage
                    .load::<serde_json::Value>()?
                    .unwrap_or(serde_json::Value::Null);
                Ok(ToolOutput {
                    content: object.to_string(),
                    ok: true,
                })
            })
        }
    }

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

    impl DetachedToolHandler for DynamicDetachedTool {
        type Args = EmptyArgs;

        fn invoke<'a>(
            &'a self,
            _context: ToolContext,
            _args: Self::Args,
        ) -> DetachedToolFuture<'a> {
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

        let (join, detach) =
            ToolRunner::stateless(&tools).run(vec![joined, detached_a, detached_b]);
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

        let (join, detach) = ToolRunner::stateless(&tools).run(vec![call]);
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

    #[test]
    fn agent_storage_uses_the_resolved_tool_group_namespace() {
        let mut tools = ToolSet::empty();
        assert!(tools
            .add_group(ToolGroup::new(
                "shared",
                true,
                [
                    Tool::new(StoreTool),
                    Tool::new(LoadTool {
                        name: "shared_load",
                    }),
                ],
            ))
            .is_ok());
        assert!(tools
            .add_group(ToolGroup::new(
                "isolated",
                true,
                [Tool::new(LoadTool {
                    name: "isolated_load",
                })],
            ))
            .is_ok());
        let Ok(tools) = tools.begin() else {
            return;
        };
        let backend = Arc::new(MemoryAgentStorage::default());
        let storage = AgentStorageScope::new(backend);

        let Ok(store) = ToolInvocation::try_new(Some("store"), "store", "{}") else {
            return;
        };
        let (stored, _) = ToolRunner::with_storage(&tools, storage.clone()).run(vec![store]);
        let stored = block_on(stored.collect::<Vec<_>>());
        assert!(stored.first().is_some_and(|(_, output)| output.ok));

        let Ok(shared) = ToolInvocation::try_new(Some("shared"), "shared_load", "{}") else {
            return;
        };
        let (shared, _) = ToolRunner::with_storage(&tools, storage.clone()).run(vec![shared]);
        let shared = block_on(shared.collect::<Vec<_>>());
        assert!(shared
            .first()
            .is_some_and(|(_, output)| output.content == r#"{"value":7}"#));

        let Ok(isolated) = ToolInvocation::try_new(Some("isolated"), "isolated_load", "{}") else {
            return;
        };
        let (isolated, _) = ToolRunner::with_storage(&tools, storage).run(vec![isolated]);
        let isolated = block_on(isolated.collect::<Vec<_>>());
        assert!(isolated
            .first()
            .is_some_and(|(_, output)| output.content == "null"));
    }
}

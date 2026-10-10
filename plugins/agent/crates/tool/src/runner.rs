use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_runtime_utils::unordered::Unordered;
use futures_core::Stream;
use portable_atomic::{AtomicU32, Ordering};
use tracing::Instrument as _;

use super::{
    background, BackgroundToolPool, Tool, ToolError, ToolInvocation, ToolOutput, ToolResult,
    ToolSetHandle,
};

type ToolRunFuture = Pin<Box<dyn Future<Output = (ToolInvocation, ToolOutput)> + 'static>>;

static NEXT_TOOL_TASK_ID: AtomicU32 = AtomicU32::new(0);

/// Stream of model-facing results for one dispatched tool batch.
///
/// Ordinary calls produce their real result. Background calls produce their
/// accepted output once their work has moved into the background pool.
pub struct ToolJoinHandle {
    runs: Unordered<ToolRunFuture>,
}

impl ToolJoinHandle {
    pub fn merge(&mut self, other: Self) {
        self.runs.extend(other.runs);
    }
}

impl Stream for ToolJoinHandle {
    type Item = (ToolInvocation, ToolOutput);

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.runs).poll_next(context)
    }
}

/// Dispatches one authorized tool batch without polling any invocation.
pub struct ToolRunner<'a> {
    tools: &'a ToolSetHandle<'a>,
    background: Option<&'a BackgroundToolPool>,
}

impl<'a> ToolRunner<'a> {
    /// A runner that rejects background Tools.
    pub fn new(tools: &'a ToolSetHandle<'a>) -> Self {
        Self {
            tools,
            background: None,
        }
    }

    /// Accepted background calls move into `pool`.
    #[must_use]
    pub fn with_background(mut self, pool: &'a BackgroundToolPool) -> Self {
        self.background = Some(pool);
        self
    }

    pub fn run(&self, calls: Vec<ToolInvocation>) -> ToolJoinHandle {
        let mut runs = Unordered::new();
        for invocation in calls {
            let span = toolcall_span(&invocation);
            let run = match self.tools.runnable_tool(&invocation) {
                Ok(tool) if tool.is_background() => {
                    start_background(tool, invocation, self.background.cloned(), span)
                }
                Ok(tool) => run(tool, invocation, span),
                Err(error) => blocked(invocation, error, span),
            };
            runs.push(run);
        }
        ToolJoinHandle { runs }
    }
}

fn blocked(
    invocation: ToolInvocation,
    error: crate::ToolInvokeError,
    span: tracing::Span,
) -> ToolRunFuture {
    Box::pin(
        async move {
            let output = settle(Err(error));
            trace_result(&output, true);
            (invocation, output)
        }
        .instrument(span),
    )
}

fn run(tool: Tool, invocation: ToolInvocation, span: tracing::Span) -> ToolRunFuture {
    Box::pin(
        async move {
            let output = settle(tool.invoke(&invocation).await);
            trace_result(&output, false);
            (invocation, output)
        }
        .instrument(span),
    )
}

fn start_background(
    tool: Tool,
    invocation: ToolInvocation,
    pool: Option<BackgroundToolPool>,
    span: tracing::Span,
) -> ToolRunFuture {
    let call_span = span.clone();
    Box::pin(
        async move {
            let started = match pool {
                Some(pool) => tool
                    .invoke_background(&invocation)
                    .await
                    .map(|background| (pool, background)),
                None => Err(ToolError::InvokeRejected(
                    "background tools are unavailable here".to_owned(),
                )
                .into()),
            };
            let output = match started {
                Ok((pool, background)) => {
                    background::start(&pool, invocation.clone(), background, call_span)
                }
                Err(error) => {
                    let output = settle(Err(error));
                    trace_result(&output, false);
                    output
                }
            };
            (invocation, output)
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

pub(crate) fn trace_result(output: &ToolOutput, blocked: bool) {
    if output.ok {
        tracing::info!(name: "result", ok = output.ok, blocked);
    } else {
        tracing::warn!(name: "result", ok = output.ok, blocked);
    }
}

pub(crate) fn settle(output: ToolResult<ToolOutput>) -> ToolOutput {
    match output {
        Ok(output) => output,
        Err(error) => ToolOutput {
            content: error.to_string(),
            ok: false,
        },
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use alloc::vec;
    use barracuda_runtime_utils::background::{BackgroundError, BackgroundUpdate};
    use futures_lite::future::{block_on, poll_fn};
    use futures_lite::StreamExt as _;

    use super::*;
    use crate::{
        BackgroundTool, BackgroundToolCall, BackgroundToolControl, BackgroundToolFuture,
        BackgroundToolHandler, EmptyArgs, ToolFuture, ToolGroup, ToolHandler, ToolInvokeError,
        ToolSet, ToolSpec,
    };

    macro_rules! object_spec {
        ($name:literal) => {
            fn name(&self) -> &str {
                $name
            }

            fn schema(&self) -> &str {
                concat!(
                    r#"{"type":"function","function":{"name":""#,
                    $name,
                    r#"","parameters":{"type":"object"}}}"#
                )
            }

            fn arguments_validator(&self) -> &'static json_validator::Validator {
                const VALIDATOR: json_validator::Validator =
                    json_validator::validator!("tests/fixtures/object.json");
                &VALIDATOR
            }
        };
    }

    struct EchoTool;

    impl ToolSpec for EchoTool {
        object_spec!("echo");
    }

    impl ToolHandler for EchoTool {
        type Args = EmptyArgs;

        fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
            Box::pin(async {
                Ok(ToolOutput {
                    content: "echo".to_owned(),
                    ok: true,
                })
            })
        }
    }

    /// Completes immediately after publishing one progress update.
    struct ProgressingTool;

    impl ToolSpec for ProgressingTool {
        object_spec!("progressing");
    }

    impl BackgroundToolHandler for ProgressingTool {
        type Args = EmptyArgs;

        fn invoke<'a>(&'a self, _args: Self::Args) -> BackgroundToolFuture<'a> {
            Box::pin(async {
                Ok(BackgroundTool::with_progress(
                    output("accepted"),
                    |progress| {
                        Box::pin(async move {
                            progress.send(output("input-required"));
                            Ok(output("completed"))
                        })
                    },
                ))
            })
        }
    }

    /// Never completes; reports a status and accepts input.
    struct EndlessTool;

    struct EndlessControl;

    impl BackgroundToolControl for EndlessControl {
        fn status(&self) -> Option<alloc::string::String> {
            Some("input_required".to_owned())
        }

        fn input<'a>(&'a self, input: Option<alloc::string::String>) -> ToolFuture<'a> {
            Box::pin(async move { Ok(output(&input.unwrap_or_default())) })
        }
    }

    impl ToolSpec for EndlessTool {
        object_spec!("endless");
    }

    impl BackgroundToolHandler for EndlessTool {
        type Args = EmptyArgs;

        fn invoke<'a>(&'a self, _args: Self::Args) -> BackgroundToolFuture<'a> {
            Box::pin(async {
                Ok(
                    BackgroundTool::new(output("accepted"), Box::pin(core::future::pending()))
                        .with_control(EndlessControl),
                )
            })
        }
    }

    fn output(content: &str) -> ToolOutput {
        ToolOutput {
            content: content.to_owned(),
            ok: true,
        }
    }

    fn call(id: &str, name: &str) -> ToolInvocation {
        ToolInvocation::try_new(Some(id), name, "{}").expect("valid invocation")
    }

    fn tool_set() -> ToolSet {
        let mut tools = ToolSet::empty();
        tools
            .add_group(ToolGroup::new(
                "test",
                true,
                [
                    Tool::new(EchoTool),
                    Tool::background(ProgressingTool),
                    Tool::background(EndlessTool),
                ],
            ))
            .expect("test group registers");
        tools
    }

    #[test]
    fn background_calls_move_into_the_pool_with_their_accepted_output() {
        let mut tools = tool_set();
        let tools = tools.begin().expect("tool set begins");
        let pool = BackgroundToolPool::new();
        let join = ToolRunner::new(&tools)
            .with_background(&pool)
            .run(vec![call("call-1", "echo"), call("call-2", "progressing")]);
        let results = block_on(join.collect::<Vec<_>>());

        assert!(results
            .iter()
            .any(|(call, output)| call.id() == Some("call-1") && output.content == "echo"));
        assert!(results
            .iter()
            .any(|(call, output)| call.id() == Some("call-2")
                && output.content == "[background:accepted]\nid: 1\naccepted"));

        let progress = block_on(poll_fn(|context| pool.poll_next(context)));
        assert_eq!(progress.id, 1);
        assert_eq!(progress.meta.invocation().id(), Some("call-2"));
        assert_eq!(
            progress.update,
            BackgroundUpdate::Progress(output("input-required"))
        );
        let completed = block_on(poll_fn(|context| pool.poll_next(context)));
        assert_eq!(
            completed.update,
            BackgroundUpdate::Completed(output("completed"))
        );
        assert!(pool.is_empty());
    }

    #[test]
    fn runner_without_a_pool_rejects_background_tools() {
        let mut tools = tool_set();
        let tools = tools.begin().expect("tool set begins");
        let join = ToolRunner::new(&tools).run(vec![call("call-1", "progressing")]);
        let results = block_on(join.collect::<Vec<_>>());
        assert!(matches!(results.first(), Some((_, output)) if !output.ok));
    }

    #[test]
    fn pooled_calls_report_status_take_input_and_cancel() {
        let mut tools = tool_set();
        let tools = tools.begin().expect("tool set begins");
        let pool = BackgroundToolPool::new();
        let join = ToolRunner::new(&tools).with_background(&pool).run(vec![
            call("call-1", "endless"),
            call("call-2", "progressing"),
        ]);
        let _ = block_on(join.collect::<Vec<_>>());

        let listed = pool.list();
        let status = |name: &str| {
            listed
                .iter()
                .find(|(_, call)| call.invocation().name() == name)
                .map(|(id, call)| (*id, call.status()))
                .expect("call is pooled")
        };
        let (endless, endless_status) = status("endless");
        let (progressing, progressing_status) = status("progressing");
        assert_eq!(endless_status, "input_required");
        assert_eq!(progressing_status, "running");

        let call = pool.get(endless).expect("endless call is pooled");
        let fed = block_on(call.input(Some("hello".to_owned())));
        assert!(matches!(fed, Ok(output) if output.content == "hello"));
        let call = pool.get(progressing).expect("progressing call is pooled");
        assert!(block_on(call.input(None)).is_err());

        pool.remove(endless)
            .map(BackgroundToolCall::cancel)
            .expect("endless call cancels");
        let missing = ToolInvokeError::from(pool.remove(endless).expect_err("call is gone"));
        assert_eq!(
            missing.to_string(),
            "tool invocation rejected: no running background call 1; finished results are delivered automatically"
        );
        assert_eq!(
            pool.remove(endless).err(),
            Some(BackgroundError::NotFound(endless))
        );
        assert_eq!(pool.list().len(), 1);
    }
}

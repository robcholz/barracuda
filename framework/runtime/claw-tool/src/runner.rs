use core::pin::Pin;
use core::task::{Context, Poll};

use futures_core::Stream;

use super::{
    Tool, ToolFuture, ToolInvocation, ToolInvokeError, ToolOutput, ToolResult, ToolSetHandle,
};

const DETACHED_ACCEPTED: &str = concat!(
    "[detached:accepted]\n",
    "The tool is running in the background. ",
    "Its result will be delivered automatically."
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolExecution {
    pub content: String,
    pub ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRunInvocation {
    id: Option<String>,
    name: String,
    arguments_json: String,
}

impl ToolRunInvocation {
    pub fn try_new(id: Option<&str>, name: &str, arguments_json: &str) -> ToolResult<Self> {
        let invocation = ToolInvocation::try_new(id, name, arguments_json)?;
        Ok(Self {
            id: invocation.id().map(str::to_owned),
            name: invocation.name().to_owned(),
            arguments_json: invocation.arguments_json().to_owned(),
        })
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn arguments_json(&self) -> &str {
        &self.arguments_json
    }

    pub fn as_invocation(&self) -> Result<ToolInvocation<'_>, ToolInvokeError> {
        ToolInvocation::try_new(
            self.id.as_deref(),
            self.name.as_str(),
            self.arguments_json.as_str(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRunResult {
    invocation: ToolRunInvocation,
    execution: ToolExecution,
}

impl ToolRunResult {
    pub fn into_parts(self) -> (ToolRunInvocation, ToolExecution) {
        (self.invocation, self.execution)
    }
}

#[derive(Debug)]
pub struct ToolDetachCompletion {
    invocation: ToolRunInvocation,
    execution: ToolExecution,
}

impl ToolDetachCompletion {
    pub fn into_parts(self) -> (ToolRunInvocation, ToolExecution) {
        (self.invocation, self.execution)
    }
}

struct PendingToolRun {
    invocation: ToolRunInvocation,
    future: ToolFuture<'static>,
}

#[derive(Default)]
struct ToolRuns {
    runs: Vec<PendingToolRun>,
    cursor: usize,
}

impl ToolRuns {
    fn push(&mut self, invocation: ToolRunInvocation, future: ToolFuture<'static>) {
        self.runs.push(PendingToolRun { invocation, future });
    }

    fn append(&mut self, other: &mut Self) {
        self.runs.append(&mut other.runs);
    }

    fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    fn poll_next(&mut self, context: &mut Context<'_>) -> Poll<Option<ToolRunResult>> {
        let count = self.runs.len();
        if count == 0 {
            return Poll::Ready(None);
        }

        let start = self.cursor % count;
        for offset in 0..count {
            let index = (start + offset) % count;
            let output = match self.runs[index].future.as_mut().poll(context) {
                Poll::Ready(output) => output,
                Poll::Pending => continue,
            };
            let run = self.runs.swap_remove(index);
            self.cursor = if self.runs.is_empty() {
                0
            } else {
                index % self.runs.len()
            };
            return Poll::Ready(Some(ToolRunResult {
                invocation: run.invocation,
                execution: execution(output),
            }));
        }

        self.cursor = (start + 1) % count;
        Poll::Pending
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
    pub fn merge(&mut self, mut other: Self) {
        self.runs.append(&mut other.runs);
    }
}

impl Stream for ToolJoinHandle {
    type Item = ToolRunResult;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.runs.poll_next(context)
    }
}

/// Stream of real completions for all detached calls in one dispatched batch.
pub struct ToolDetachHandle {
    runs: ToolRuns,
}

impl Stream for ToolDetachHandle {
    type Item = ToolDetachCompletion;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.runs.poll_next(context) {
            Poll::Ready(Some(result)) => {
                let (invocation, execution) = result.into_parts();
                Poll::Ready(Some(ToolDetachCompletion {
                    invocation,
                    execution,
                }))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
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

    pub fn run(&self, calls: Vec<ToolRunInvocation>) -> (ToolJoinHandle, Option<ToolDetachHandle>) {
        let mut joined = ToolRuns::default();
        let mut detached = ToolRuns::default();

        for call in calls {
            let invocation = call;
            let borrowed = match invocation.as_invocation() {
                Ok(call) => call,
                Err(error) => {
                    joined.push(invocation, ready(Err(error)));
                    continue;
                }
            };
            let tool = match self.tools.runnable_tool(&borrowed) {
                Ok(tool) => tool,
                Err(error) => {
                    joined.push(invocation, ready(Err(error)));
                    continue;
                }
            };
            let is_detached = tool.config().detached;
            let future = owned_tool_future(tool, invocation.clone());
            if is_detached {
                joined.push(invocation.clone(), ready(Ok(detached_accepted())));
                detached.push(invocation, future);
            } else {
                joined.push(invocation, future);
            }
        }

        let detached = (!detached.is_empty()).then_some(ToolDetachHandle { runs: detached });
        (ToolJoinHandle { runs: joined }, detached)
    }
}

fn ready(output: ToolResult<ToolOutput>) -> ToolFuture<'static> {
    Box::pin(async move { output })
}

fn detached_accepted() -> ToolOutput {
    ToolOutput {
        output: DETACHED_ACCEPTED.to_owned(),
        ok: true,
    }
}

fn owned_tool_future(tool: Tool, invocation: ToolRunInvocation) -> ToolFuture<'static> {
    Box::pin(async move {
        let call = invocation.as_invocation()?;
        tool.invoke(&call).await
    })
}

fn execution(output: ToolResult<ToolOutput>) -> ToolExecution {
    match output {
        Ok(output) => ToolExecution {
            content: output.output,
            ok: output.ok,
        },
        Err(error) => ToolExecution {
            content: error.to_string(),
            ok: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;

    use super::*;
    use crate::{SyncToolHandler, ToolConfig, ToolGroup, ToolRegistry, ToolSpec};

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
        fn invoke(&self, _call: &ToolInvocation<'_>) -> ToolResult<ToolOutput> {
            Ok(ToolOutput {
                output: self.name.to_owned(),
                ok: true,
            })
        }
    }

    #[test]
    fn run_splits_one_batch_into_join_and_detach_streams() {
        let registry = Arc::new(ToolRegistry::new());
        let mut tools = registry.tool_set();
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
        let joined = ToolRunInvocation::try_new(Some("call-1"), "joined", "{}");
        let detached_a = ToolRunInvocation::try_new(Some("call-2"), "detached_a", "{}");
        let detached_b = ToolRunInvocation::try_new(Some("call-3"), "detached_b", "{}");
        assert!(joined.is_ok());
        assert!(detached_a.is_ok());
        assert!(detached_b.is_ok());
        let (Ok(joined), Ok(detached_a), Ok(detached_b)) = (joined, detached_a, detached_b) else {
            return;
        };

        let (join, detach) = ToolRunner::new(&tools).run(vec![joined, detached_a, detached_b]);
        let joined = block_on(join.collect::<Vec<_>>());
        assert_eq!(joined.len(), 3);
        assert!(joined.iter().any(|result| {
            result.invocation.id() == Some("call-1") && result.execution.content == "joined"
        }));
        assert!(joined.iter().any(|result| {
            result.invocation.id() == Some("call-2")
                && result.execution.content.starts_with("[detached:accepted]")
        }));
        assert!(joined.iter().any(|result| {
            result.invocation.id() == Some("call-3")
                && result.execution.content.starts_with("[detached:accepted]")
        }));

        assert!(detach.is_some());
        let Some(detach) = detach else {
            return;
        };
        let detached = block_on(detach.collect::<Vec<_>>())
            .into_iter()
            .map(ToolDetachCompletion::into_parts)
            .collect::<Vec<_>>();
        assert_eq!(detached.len(), 2);
        assert!(detached.iter().any(|(invocation, execution)| {
            invocation.id() == Some("call-2") && execution.content == "detached_a" && execution.ok
        }));
        assert!(detached.iter().any(|(invocation, execution)| {
            invocation.id() == Some("call-3") && execution.content == "detached_b" && execution.ok
        }));
    }
}

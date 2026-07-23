use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::VecDeque;

use futures_core::Stream;

use super::{Tool, ToolInvocation, ToolOutput, ToolResult, ToolSetHandle};

const DETACHED_ACCEPTED: &str = concat!(
    "[detached:accepted]\n",
    "The tool is running in the background. ",
    "Its result will be delivered automatically."
);

type ToolRunFuture = Pin<Box<dyn Future<Output = (ToolInvocation, ToolOutput)> + Send + 'static>>;

#[derive(Default)]
struct ToolRuns {
    runs: VecDeque<ToolRunFuture>,
}

impl ToolRuns {
    fn push(&mut self, future: ToolRunFuture) {
        self.runs.push_back(future);
    }

    fn append(&mut self, other: &mut Self) {
        self.runs.append(&mut other.runs);
    }

    fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    fn poll_next(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Option<(ToolInvocation, ToolOutput)>> {
        let count = self.runs.len();
        for _ in 0..count {
            let Some(mut future) = self.runs.pop_front() else {
                return Poll::Ready(None);
            };
            match future.as_mut().poll(context) {
                Poll::Ready(result) => return Poll::Ready(Some(result)),
                Poll::Pending => self.runs.push_back(future),
            }
        }

        if self.runs.is_empty() {
            Poll::Ready(None)
        } else {
            Poll::Pending
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
    pub fn merge(&mut self, mut other: Self) {
        self.runs.append(&mut other.runs);
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
            let tool = match self.tools.runnable_tool(&invocation) {
                Ok(tool) => tool,
                Err(error) => {
                    joined.push(ready(invocation, Err(error)));
                    continue;
                }
            };
            if tool.config().detached {
                joined.push(ready(invocation.clone(), Ok(detached_accepted())));
                detached.push(run(tool, invocation));
            } else {
                joined.push(run(tool, invocation));
            }
        }

        let detached = (!detached.is_empty()).then_some(ToolDetachHandle { runs: detached });
        (ToolJoinHandle { runs: joined }, detached)
    }
}

fn ready(invocation: ToolInvocation, output: ToolResult<ToolOutput>) -> ToolRunFuture {
    Box::pin(async move { (invocation, settle(output)) })
}

fn run(tool: Tool, invocation: ToolInvocation) -> ToolRunFuture {
    Box::pin(async move {
        let output = tool.invoke(&invocation).await;
        (invocation, settle(output))
    })
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
    use crate::{SyncToolHandler, ToolConfig, ToolGroup, ToolSet, ToolSpec};

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
}

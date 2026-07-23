//! Runtime handoff for tools whose work may outlive the invoking Agent turn.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::sync::Arc;

use super::{ToolExecution, ToolFuture, ToolInvocation, ToolInvokeError};

/// Owned identity of one detached tool invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetachedToolInvocation {
    id: Option<String>,
    name: String,
    arguments_json: String,
}

impl DetachedToolInvocation {
    pub(crate) fn from_invocation(invocation: &ToolInvocation<'_>) -> Self {
        Self {
            id: invocation.id().map(str::to_owned),
            name: invocation.name().to_owned(),
            arguments_json: invocation.arguments_json().to_owned(),
        }
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

    pub(crate) fn as_invocation(&self) -> Result<ToolInvocation<'_>, ToolInvokeError> {
        ToolInvocation::try_new(
            self.id.as_deref(),
            self.name.as_str(),
            self.arguments_json.as_str(),
        )
    }
}

/// Completed output of one detached tool invocation.
#[derive(Debug)]
pub struct DetachedToolCompletion {
    invocation: DetachedToolInvocation,
    execution: ToolExecution,
}

impl DetachedToolCompletion {
    pub fn into_parts(self) -> (DetachedToolInvocation, ToolExecution) {
        (self.invocation, self.execution)
    }
}

/// An owned detached invocation submitted to an external runtime.
pub struct DetachedToolRun {
    invocation: Option<DetachedToolInvocation>,
    future: ToolFuture<'static>,
}

impl DetachedToolRun {
    pub(crate) fn new(invocation: DetachedToolInvocation, future: ToolFuture<'static>) -> Self {
        Self {
            invocation: Some(invocation),
            future,
        }
    }
}

impl Future for DetachedToolRun {
    type Output = DetachedToolCompletion;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let output = match self.future.as_mut().poll(context) {
            Poll::Ready(output) => output,
            Poll::Pending => return Poll::Pending,
        };
        let execution = match output {
            Ok(output) => ToolExecution {
                content: output.output,
                ok: output.ok,
            },
            Err(error) => ToolExecution {
                content: error.to_string(),
                ok: false,
            },
        };
        let invocation = self
            .invocation
            .take()
            .expect("a detached tool run completes only once");
        Poll::Ready(DetachedToolCompletion {
            invocation,
            execution,
        })
    }
}

type SubmitDetached =
    dyn Fn(DetachedToolRun) -> Result<(), ToolInvokeError> + Send + Sync + 'static;

/// Cloneable submission endpoint installed by the owning runtime.
#[derive(Clone)]
pub struct DetachedToolSink {
    submit: Arc<SubmitDetached>,
}

impl DetachedToolSink {
    pub fn new(
        submit: impl Fn(DetachedToolRun) -> Result<(), ToolInvokeError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            submit: Arc::new(submit),
        }
    }

    pub(crate) fn submit(&self, run: DetachedToolRun) -> Result<(), ToolInvokeError> {
        (self.submit)(run)
    }
}

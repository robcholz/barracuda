//! Background Tools: the Tool layer over the generic [`BackgroundPool`].
//!
//! A background Tool call returns an accepted output to the current model turn
//! while its work keeps running as a task in the Agent's [`BackgroundToolPool`].
//! The pool only stores tasks under ids; [`BackgroundToolCall`] is the Tool
//! metadata it carries for each one, and owns every Tool-level behavior: the
//! invocation, status, input, cancel hook, and `toolcall` tracing.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use barracuda_runtime_utils::background::{BackgroundError, BackgroundPool, BackgroundTask};
use futures_core::Stream;
use getset::Getters;
use portable_atomic_util::{Arc, Weak};

use crate::definition::{
    ToolError, ToolFuture, ToolInvocation, ToolInvokeError, ToolOutput, ToolResult,
};
use crate::runner::{settle, trace_result};

/// The background Tool calls of one Agent, keyed by pool-assigned id.
pub type BackgroundToolPool = BackgroundPool<BackgroundToolCall, ToolOutput>;

pub type ToolCompletionFuture = Pin<Box<dyn Future<Output = ToolResult<ToolOutput>> + 'static>>;
pub type BackgroundToolFuture<'a> = Pin<Box<dyn Future<Output = ToolResult<BackgroundTool>> + 'a>>;

/// Accepted, progress, and terminal values produced by a background Tool.
///
/// The accepted output is returned to the current model turn. Optional
/// progress is delivered while the call remains in its pool. Returning from
/// the completion future implicitly completes the call.
pub struct BackgroundTool {
    accepted: ToolOutput,
    progress: Option<ToolProgressReceiver>,
    completion: ToolCompletionFuture,
    control: Option<Rc<dyn BackgroundToolControl>>,
}

impl BackgroundTool {
    pub fn new(accepted: ToolOutput, completion: ToolCompletionFuture) -> Self {
        Self {
            accepted,
            progress: None,
            completion,
            control: None,
        }
    }

    /// Creates a background Tool whose handler can publish non-terminal
    /// updates.
    ///
    /// Returning from the completion future remains the only completion
    /// signal; the progress sender cannot complete the call.
    pub fn with_progress(
        accepted: ToolOutput,
        completion: impl FnOnce(ToolProgressSender) -> ToolCompletionFuture,
    ) -> Self {
        let (sender, progress) = tool_progress_channel();
        Self {
            accepted,
            progress: Some(progress),
            completion: completion(sender),
            control: None,
        }
    }

    /// Lets the owning pool report status, deliver input, and cancel work the
    /// completion future does not own.
    #[must_use]
    pub fn with_control(mut self, control: impl BackgroundToolControl + 'static) -> Self {
        self.control = Some(Rc::new(control));
        self
    }
}

/// Domain hooks a background Tool exposes to its [`BackgroundToolPool`].
///
/// Dropping the completion future always stops the pool from observing the
/// call; these hooks cover what the drop alone cannot express.
pub trait BackgroundToolControl {
    /// Short model-facing state. `None` reports the call as `running`.
    fn status(&self) -> Option<String> {
        None
    }

    /// Delivers one model-supplied input value, or the end of input as `None`.
    fn input<'a>(&'a self, _input: Option<String>) -> ToolFuture<'a> {
        Box::pin(async {
            Err(
                ToolError::InvokeRejected("this background tool does not accept input".to_owned())
                    .into(),
            )
        })
    }

    /// Stops work that outlives the dropped completion future.
    fn cancel(&self) {}
}

/// Non-terminal update channel owned by a background Tool handler.
#[derive(Clone)]
pub struct ToolProgressSender {
    state: Weak<ToolProgressState>,
}

impl ToolProgressSender {
    /// Publishes one best-effort progress update.
    ///
    /// Updates are ignored after the owning pool stops observing the call.
    pub fn send(&self, output: ToolOutput) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut queued = state.queued.borrow_mut();
        if queued.len() >= TOOL_PROGRESS_CAPACITY {
            return;
        }
        queued.push_back(output);
        drop(queued);
        if let Some(waker) = state.waker.borrow_mut().take() {
            waker.wake();
        };
    }
}

const TOOL_PROGRESS_CAPACITY: usize = 8;

struct ToolProgressState {
    queued: RefCell<VecDeque<ToolOutput>>,
    waker: RefCell<Option<Waker>>,
}

struct ToolProgressReceiver {
    state: Arc<ToolProgressState>,
}

impl Stream for ToolProgressReceiver {
    type Item = ToolOutput;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(output) = self.state.queued.borrow_mut().pop_front() {
            return Poll::Ready(Some(output));
        }
        *self.state.waker.borrow_mut() = Some(context.waker().clone());
        Poll::Pending
    }
}

fn tool_progress_channel() -> (ToolProgressSender, ToolProgressReceiver) {
    let state = Arc::new(ToolProgressState {
        queued: RefCell::new(VecDeque::new()),
        waker: RefCell::new(None),
    });
    (
        ToolProgressSender {
            state: Arc::downgrade(&state),
        },
        ToolProgressReceiver { state },
    )
}

/// Tool metadata of one call in a [`BackgroundToolPool`].
#[derive(Clone, Getters)]
pub struct BackgroundToolCall {
    /// The model-requested call that started the work.
    #[getset(get = "pub")]
    invocation: ToolInvocation,
    control: Option<Rc<dyn BackgroundToolControl>>,
    /// The `toolcall` span stays open while the call is pooled.
    span: tracing::Span,
}

impl core::fmt::Debug for BackgroundToolCall {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("BackgroundToolCall")
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

impl BackgroundToolCall {
    /// Short model-facing state, `running` unless the Tool reports one.
    pub fn status(&self) -> String {
        self.control
            .as_ref()
            .and_then(|control| control.status())
            .unwrap_or_else(|| "running".to_owned())
    }

    /// Delivers model-supplied input, or the end of input as `None`.
    pub async fn input(&self, input: Option<String>) -> ToolResult<ToolOutput> {
        match &self.control {
            Some(control) => control.input(input).await,
            None => Err(ToolError::InvokeRejected(format!(
                "background tool {} does not accept input",
                self.invocation.name()
            ))
            .into()),
        }
    }

    /// Finishes cancelling a call the pool has already dropped.
    pub fn cancel(self) {
        if let Some(control) = &self.control {
            control.cancel();
        }
        self.span
            .in_scope(|| tracing::info!(name: "cancelled", checkpoint = "background_cancel"));
    }
}

impl From<BackgroundError> for ToolInvokeError {
    fn from(error: BackgroundError) -> Self {
        let message = match error {
            BackgroundError::NotFound(id) => format!(
                "no running background call {id}; finished results are delivered automatically"
            ),
            BackgroundError::AlreadyWaited(id) => {
                format!("background call {id} is already being waited for")
            }
        };
        ToolError::InvokeRejected(message).into()
    }
}

/// Moves one accepted call into `pool` and returns its model-facing accepted
/// output, which carries the pool-assigned id.
pub(crate) fn start(
    pool: &BackgroundToolPool,
    invocation: ToolInvocation,
    tool: BackgroundTool,
    span: tracing::Span,
) -> ToolOutput {
    let BackgroundTool {
        accepted,
        progress,
        completion,
        control,
    } = tool;
    let completion_span = span.clone();
    let mut task = BackgroundTask::new(async move {
        let output = settle(completion.await);
        completion_span.in_scope(|| trace_result(&output, false));
        output
    });
    if let Some(progress) = progress {
        task = task.with_progress(progress);
    }
    let id = pool.insert(
        BackgroundToolCall {
            invocation,
            control,
            span,
        },
        task,
    );
    ToolOutput {
        content: format!("[background:accepted]\nid: {id}\n{}", accepted.content),
        ok: accepted.ok,
    }
}

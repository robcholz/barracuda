//! Background Tool calls and the per-Agent pool that owns them.
//!
//! A background Tool call returns an accepted output to the current model turn
//! while its work keeps running. The [`BackgroundToolPool`] owns every accepted
//! call of one Agent under a pool-assigned id, delivers its progress and
//! terminal completion, and lets the Agent list, wait for, feed, and cancel it.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use futures_core::Stream;
use portable_atomic_util::{Arc, Weak};
use serde::Serialize;

use crate::definition::{ToolError, ToolFuture, ToolInvocation, ToolOutput, ToolResult};
use crate::runner::{settle, trace_result};

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

/// One update emitted by a call in a [`BackgroundToolPool`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackgroundToolUpdate {
    /// Non-terminal information; the call remains in the pool.
    Progress(ToolOutput),
    /// Terminal result produced when the handler's completion future returns.
    Completed(ToolOutput),
}

/// One update together with the pool call that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundToolEvent {
    /// Pool-assigned id returned in the accepted output.
    pub id: u32,
    /// The model-requested call that started the work.
    pub invocation: ToolInvocation,
    pub update: BackgroundToolUpdate,
}

/// Snapshot of one running call, as listed to the model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackgroundToolInfo {
    pub id: u32,
    pub tool: String,
    pub status: String,
}

/// The background Tool calls owned by one Agent.
///
/// Cloning shares the pool. It is executor-local: the Agent drives updates
/// through [`poll_next`](Self::poll_next) while its Tools manipulate calls by
/// id. Dropping a call drops its completion future, which cancels work whose
/// handle the future owns.
#[derive(Clone, Default)]
pub struct BackgroundToolPool {
    state: Rc<RefCell<PoolState>>,
}

#[derive(Default)]
struct PoolState {
    calls: Vec<BackgroundCall>,
    last_id: u32,
    next_poll: usize,
    waker: Option<Waker>,
}

struct BackgroundCall {
    id: u32,
    invocation: ToolInvocation,
    progress: Option<ToolProgressReceiver>,
    completion: Option<ToolCompletionFuture>,
    terminal: Option<ToolOutput>,
    control: Option<Rc<dyn BackgroundToolControl>>,
    /// A `background_wait` call owns the terminal result instead of the Agent.
    waited: bool,
    /// The `toolcall` span stays open until the call leaves the pool.
    span: tracing::Span,
}

impl BackgroundCall {
    fn poll_completion(&mut self, context: &mut Context<'_>) {
        let Some(completion) = self.completion.as_mut() else {
            return;
        };
        if let Poll::Ready(output) = completion.as_mut().poll(context) {
            let output = settle(output);
            self.span.in_scope(|| trace_result(&output, false));
            self.completion = None;
            self.terminal = Some(output);
        }
    }

    fn poll_progress(&mut self, context: &mut Context<'_>) -> Poll<ToolOutput> {
        let Some(progress) = self.progress.as_mut() else {
            return Poll::Pending;
        };
        match Pin::new(progress).poll_next(context) {
            Poll::Ready(Some(output)) => Poll::Ready(output),
            Poll::Ready(None) => {
                self.progress = None;
                Poll::Pending
            }
            Poll::Pending => Poll::Pending,
        }
    }

    /// Progress queued before completion is delivered before the terminal
    /// result. A waited call reports progress only.
    fn poll_update(&mut self, context: &mut Context<'_>) -> Poll<BackgroundToolUpdate> {
        if !self.waited {
            self.poll_completion(context);
        }
        if let Poll::Ready(output) = self.poll_progress(context) {
            return Poll::Ready(BackgroundToolUpdate::Progress(output));
        }
        if self.waited {
            return Poll::Pending;
        }
        match self.terminal.take() {
            Some(output) => Poll::Ready(BackgroundToolUpdate::Completed(output)),
            None => Poll::Pending,
        }
    }
}

impl BackgroundToolPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no call is running.
    pub fn is_empty(&self) -> bool {
        self.state.borrow().calls.is_empty()
    }

    /// Takes ownership of one accepted call and returns its model-facing
    /// accepted output, which carries the pool-assigned id.
    pub(crate) fn insert(
        &self,
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
        let mut state = self.state.borrow_mut();
        state.last_id = state.last_id.wrapping_add(1);
        let id = state.last_id;
        state.calls.push(BackgroundCall {
            id,
            invocation,
            progress,
            completion: Some(completion),
            terminal: None,
            control,
            waited: false,
            span,
        });
        let waker = state.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
        ToolOutput {
            content: format!("[background:accepted]\nid: {id}\n{}", accepted.content),
            ok: accepted.ok,
        }
    }

    /// Polls every call for its next update, starting after the call that
    /// last made progress so none is starved.
    ///
    /// An empty pool stays pending; inserting a call wakes it.
    pub fn poll_next(&self, context: &mut Context<'_>) -> Poll<BackgroundToolEvent> {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        if !state
            .waker
            .as_ref()
            .is_some_and(|waker| waker.will_wake(context.waker()))
        {
            state.waker = Some(context.waker().clone());
        }
        let count = state.calls.len();
        let start = state.next_poll.min(count);
        for index in (start..count).chain(0..start) {
            let Some(call) = state.calls.get_mut(index) else {
                continue;
            };
            let Poll::Ready(update) = call.poll_update(context) else {
                continue;
            };
            let event = BackgroundToolEvent {
                id: call.id,
                invocation: call.invocation.clone(),
                update,
            };
            if matches!(event.update, BackgroundToolUpdate::Completed(_)) {
                state.calls.remove(index);
                state.next_poll = index;
            } else {
                state.next_poll = index.wrapping_add(1);
            }
            return Poll::Ready(event);
        }
        Poll::Pending
    }

    /// Snapshots every running call in start order.
    pub fn list(&self) -> Vec<BackgroundToolInfo> {
        let calls = self
            .state
            .borrow()
            .calls
            .iter()
            .map(|call| {
                (
                    call.id,
                    call.invocation.name().to_owned(),
                    call.control.clone(),
                )
            })
            .collect::<Vec<_>>();
        // Status hooks run outside the pool borrow.
        calls
            .into_iter()
            .map(|(id, tool, control)| BackgroundToolInfo {
                id,
                tool,
                status: control
                    .and_then(|control| control.status())
                    .unwrap_or_else(|| "running".to_owned()),
            })
            .collect()
    }

    /// Waits for one call's terminal result, taking it from the pool so the
    /// Agent does not deliver it a second time.
    ///
    /// Dropping the returned future before completion hands the terminal
    /// result back to automatic delivery.
    pub fn wait(&self, id: u32) -> ToolResult<BackgroundToolWait> {
        let mut state = self.state.borrow_mut();
        let call = state
            .calls
            .iter_mut()
            .find(|call| call.id == id)
            .ok_or_else(|| not_running(id))?;
        if call.waited {
            return Err(ToolError::InvokeRejected(format!(
                "background call {id} is already being waited for"
            ))
            .into());
        }
        call.waited = true;
        Ok(BackgroundToolWait {
            pool: self.clone(),
            id,
        })
    }

    /// Delivers input to one running call through its control hook.
    pub async fn input(&self, id: u32, input: Option<String>) -> ToolResult<ToolOutput> {
        let control = {
            let state = self.state.borrow();
            let call = state
                .calls
                .iter()
                .find(|call| call.id == id)
                .ok_or_else(|| not_running(id))?;
            call.control.clone().ok_or_else(|| {
                ToolError::InvokeRejected(format!(
                    "background call {id} ({}) does not accept input",
                    call.invocation.name()
                ))
            })?
        };
        control.input(input).await
    }

    /// Cancels one running call. Its terminal result is never delivered.
    pub fn cancel(&self, id: u32) -> ToolResult<()> {
        let call = {
            let mut state = self.state.borrow_mut();
            let index = state
                .calls
                .iter()
                .position(|call| call.id == id)
                .ok_or_else(|| not_running(id))?;
            state.calls.remove(index)
        };
        if let Some(control) = &call.control {
            control.cancel();
        }
        call.span
            .in_scope(|| tracing::info!(name: "cancelled", checkpoint = "background_cancel"));
        drop(call);
        Ok(())
    }

    /// Drops every call without running cancel hooks.
    pub fn clear(&self) {
        let calls = core::mem::take(&mut self.state.borrow_mut().calls);
        drop(calls);
    }

    fn wake(&self) {
        let waker = self.state.borrow_mut().waker.take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// Future returned by [`BackgroundToolPool::wait`].
pub struct BackgroundToolWait {
    pool: BackgroundToolPool,
    id: u32,
}

impl Future for BackgroundToolWait {
    type Output = ToolResult<ToolOutput>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.pool.state.borrow_mut();
        let Some(index) = state.calls.iter().position(|call| call.id == self.id) else {
            return Poll::Ready(Err(not_running(self.id)));
        };
        let Some(call) = state.calls.get_mut(index) else {
            return Poll::Ready(Err(not_running(self.id)));
        };
        call.poll_completion(context);
        let Some(output) = call.terminal.take() else {
            return Poll::Pending;
        };
        let call = state.calls.remove(index);
        drop(state);
        drop(call);
        Poll::Ready(Ok(output))
    }
}

impl Drop for BackgroundToolWait {
    fn drop(&mut self) {
        let released = self
            .pool
            .state
            .borrow_mut()
            .calls
            .iter_mut()
            .find(|call| call.id == self.id)
            .map(|call| call.waited = false)
            .is_some();
        if released {
            self.pool.wake();
        }
    }
}

fn not_running(id: u32) -> crate::ToolInvokeError {
    ToolError::InvokeRejected(format!(
        "no running background call {id}; finished results are delivered automatically"
    ))
    .into()
}

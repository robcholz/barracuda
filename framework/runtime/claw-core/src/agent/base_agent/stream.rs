use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_channel::{Receiver, Sender};
use claw_api::ToolCall;
use futures_core::Stream;
use futures_lite::future;

use crate::protocol::{InflightToolCall, IterationId};

use super::iteration_loop::{IterationLoopError, ToolCallId};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AgentSubmitError {
    #[error("cannot submit a message while the agent is running")]
    Running,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AgentApprovalError {
    #[error("agent is not awaiting approval")]
    NotAwaitingApproval,
    #[error("approval is for tool call {received}, expected {expected}")]
    ToolCallMismatch {
        expected: ToolCallId,
        received: ToolCallId,
    },
}

#[derive(Clone, Debug, strum::IntoStaticStr, PartialEq, Eq)]
pub(crate) enum ApprovalDecision {
    #[strum(serialize = "approved")]
    Approved,
    #[strum(serialize = "rejected")]
    Rejected(String),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AgentError {
    #[error(transparent)]
    Iteration(#[from] IterationLoopError),
    #[error("multiple task effects were emitted in one tool round: {count}")]
    ConflictingEffects { count: usize },
    #[error("agent run-state invariant violated")]
    StateInvariant,
}

/// Every observable value produced by one submitted Agent task.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentProgress {
    IterationStarted(IterationId),
    ReasoningDelta(String),
    ReasoningEnded,
    OutputDelta(String),
    OutputEnded,
    ToolCall(ToolCall),
    ToolCallsEnded,
    #[cfg(feature = "cache_profile")]
    Usage(claw_api::ApiUsage),
    IterationEnded,
    /// The calls are now visible to the owner. They cannot execute until the
    /// owner polls the stream again.
    ToolCalls(Vec<InflightToolCall>),
    ApprovalRequired {
        tool_call_id: ToolCallId,
        summary: String,
    },
    Yielded {
        text: String,
    },
    YieldedByTool {
        text: String,
    },
    Ended {
        final_message: String,
    },
    Interrupted,
    Cancelled,
    Failed(AgentError),
}

impl AgentProgress {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Yielded { .. }
                | Self::YieldedByTool { .. }
                | Self::Ended { .. }
                | Self::Interrupted
                | Self::Cancelled
                | Self::Failed(_)
        )
    }
}

pub(super) struct ProgressEnvelope {
    pub(super) progress: AgentProgress,
    pub(super) resume: Sender<()>,
}

#[derive(Clone)]
pub(super) struct ProgressEmitter {
    sender: Sender<ProgressEnvelope>,
}

impl ProgressEmitter {
    /// Emit exactly one stream item and wait until the consumer asks for the
    /// next item. This makes semantic boundaries real: tools cannot start in
    /// the same poll that exposes their calls.
    pub(super) async fn send(&self, progress: AgentProgress) {
        let (resume, resumed) = async_channel::bounded(1);
        if self
            .sender
            .send(ProgressEnvelope { progress, resume })
            .await
            .is_ok()
        {
            let _ = resumed.recv().await;
        }
    }
}

#[derive(Clone)]
pub(super) struct RunControl {
    inner: Rc<RunSignals>,
}

struct RunSignals {
    interrupt: Cell<bool>,
    cancel: AtomicBool,
    approval: RefCell<ApprovalState>,
    waker: RefCell<Option<Waker>>,
}

enum ApprovalState {
    Idle,
    Waiting(ToolCallId),
    Resolved(ApprovalDecision),
}

pub(super) enum ApprovalOutcome {
    Decision(ApprovalDecision),
    Interrupted,
    Cancelled,
}

impl RunControl {
    fn new() -> Self {
        Self {
            inner: Rc::new(RunSignals {
                interrupt: Cell::new(false),
                cancel: AtomicBool::new(false),
                approval: RefCell::new(ApprovalState::Idle),
                waker: RefCell::new(None),
            }),
        }
    }

    pub(super) fn cancel_flag(&self) -> &AtomicBool {
        &self.inner.cancel
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.inner.cancel.load(Ordering::Acquire)
    }

    pub(super) fn take_interrupt(&self) -> bool {
        self.inner.interrupt.replace(false)
    }

    pub(super) fn begin_approval(&self, tool_call_id: ToolCallId) {
        let mut approval = self.inner.approval.borrow_mut();
        debug_assert!(matches!(&*approval, ApprovalState::Idle));
        *approval = ApprovalState::Waiting(tool_call_id);
    }

    pub(super) async fn approval(&self) -> ApprovalOutcome {
        future::poll_fn(|context| {
            if self.is_cancelled() {
                self.clear_approval();
                return Poll::Ready(ApprovalOutcome::Cancelled);
            }
            if self.take_interrupt() {
                self.clear_approval();
                return Poll::Ready(ApprovalOutcome::Interrupted);
            }
            if let Some(decision) = self.take_approval_decision() {
                return Poll::Ready(ApprovalOutcome::Decision(decision));
            }
            self.register(context.waker());
            Poll::Pending
        })
        .await
    }

    fn interrupt(&self) {
        self.inner.interrupt.set(true);
        self.wake();
    }

    fn cancel(&self) {
        self.inner.cancel.store(true, Ordering::Release);
        self.wake();
    }

    fn resolve_approval(
        &self,
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    ) -> Result<(), AgentApprovalError> {
        let mut approval = self.inner.approval.borrow_mut();
        match &*approval {
            ApprovalState::Idle | ApprovalState::Resolved(_) => {
                return Err(AgentApprovalError::NotAwaitingApproval);
            }
            ApprovalState::Waiting(expected) if *expected != tool_call_id => {
                return Err(AgentApprovalError::ToolCallMismatch {
                    expected: *expected,
                    received: tool_call_id,
                });
            }
            ApprovalState::Waiting(_) => {}
        }
        *approval = ApprovalState::Resolved(decision);
        drop(approval);
        self.wake();
        Ok(())
    }

    fn clear_approval(&self) {
        *self.inner.approval.borrow_mut() = ApprovalState::Idle;
    }

    fn take_approval_decision(&self) -> Option<ApprovalDecision> {
        let mut approval = self.inner.approval.borrow_mut();
        if !matches!(&*approval, ApprovalState::Resolved(_)) {
            return None;
        }
        let ApprovalState::Resolved(decision) =
            std::mem::replace(&mut *approval, ApprovalState::Idle)
        else {
            unreachable!("checked approval state changed while exclusively borrowed")
        };
        Some(decision)
    }

    fn register(&self, waker: &Waker) {
        let mut registered = self.inner.waker.borrow_mut();
        if registered
            .as_ref()
            .is_none_or(|current| !current.will_wake(waker))
        {
            *registered = Some(waker.clone());
        }
    }

    fn wake(&self) {
        if let Some(waker) = self.inner.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

type AgentDriver<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// The unique mutable capability for one submitted BaseAgent task.
pub(crate) struct AgentStreamHandle<'a> {
    driver: Option<AgentDriver<'a>>,
    progress: Pin<Box<Receiver<ProgressEnvelope>>>,
    resume: Option<Sender<()>>,
    control: RunControl,
}

impl<'a> AgentStreamHandle<'a> {
    pub(super) fn new(
        driver: AgentDriver<'a>,
        progress: Receiver<ProgressEnvelope>,
        control: RunControl,
    ) -> Self {
        Self {
            driver: Some(driver),
            progress: Box::pin(progress),
            resume: None,
            control,
        }
    }

    pub(super) fn channel() -> (ProgressEmitter, Receiver<ProgressEnvelope>) {
        let (sender, receiver) = async_channel::bounded(1);
        (ProgressEmitter { sender }, receiver)
    }

    pub(super) fn control() -> RunControl {
        RunControl::new()
    }

    pub(crate) fn interrupt(&mut self) {
        self.control.interrupt();
    }

    pub(crate) fn cancel(&mut self) {
        self.control.cancel();
    }

    pub(crate) fn resolve_approval(
        &mut self,
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    ) -> Result<(), AgentApprovalError> {
        self.control.resolve_approval(tool_call_id, decision)
    }

    fn take_progress(&mut self, context: &mut Context<'_>) -> Poll<Option<AgentProgress>> {
        match self.progress.as_mut().poll_next(context) {
            Poll::Ready(Some(envelope)) => {
                self.resume = Some(envelope.resume);
                Poll::Ready(Some(envelope.progress))
            }
            Poll::Ready(None) if self.driver.is_none() => Poll::Ready(None),
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }
}

impl Stream for AgentStreamHandle<'_> {
    type Item = AgentProgress;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if let Some(resume) = this.resume.take() {
            let _ = resume.try_send(());
        }

        if let Poll::Ready(progress) = this.take_progress(context) {
            return Poll::Ready(progress);
        }

        if let Some(driver) = this.driver.as_mut() {
            if driver.as_mut().poll(context).is_ready() {
                this.driver = None;
            }
        }

        this.take_progress(context)
    }
}

impl Drop for AgentStreamHandle<'_> {
    fn drop(&mut self) {
        self.control.cancel();
        if let Some(resume) = self.resume.take() {
            let _ = resume.try_send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;

    use super::*;

    #[test]
    fn progress_is_a_real_poll_boundary() {
        let (progress, receiver) = AgentStreamHandle::channel();
        let control = AgentStreamHandle::control();
        let phase = Rc::new(Cell::new(0));
        let producer_phase = Rc::clone(&phase);
        let driver = Box::pin(async move {
            progress.send(AgentProgress::ToolCalls(Vec::new())).await;
            producer_phase.set(1);
            progress.send(AgentProgress::Cancelled).await;
            producer_phase.set(2);
        });
        let mut stream = AgentStreamHandle::new(driver, receiver, control);

        block_on(async {
            assert_eq!(
                stream.next().await,
                Some(AgentProgress::ToolCalls(Vec::new()))
            );
            assert_eq!(phase.get(), 0, "producer remains parked at the boundary");

            assert_eq!(stream.next().await, Some(AgentProgress::Cancelled));
            assert_eq!(phase.get(), 1, "the next poll resumes the producer once");

            assert_eq!(stream.next().await, None);
            assert_eq!(phase.get(), 2);
        });
    }

    #[test]
    fn approval_is_resolved_through_the_stream_handle() {
        let (progress, receiver) = AgentStreamHandle::channel();
        let control = AgentStreamHandle::control();
        let driver_control = control.clone();
        let driver = Box::pin(async move {
            driver_control.begin_approval(ToolCallId::new(0));
            progress
                .send(AgentProgress::ApprovalRequired {
                    tool_call_id: ToolCallId::new(0),
                    summary: "run tool".to_owned(),
                })
                .await;
            let terminal = match driver_control.approval().await {
                ApprovalOutcome::Decision(ApprovalDecision::Approved) => AgentProgress::Ended {
                    final_message: "approved".to_owned(),
                },
                ApprovalOutcome::Decision(ApprovalDecision::Rejected(_)) => {
                    AgentProgress::Cancelled
                }
                ApprovalOutcome::Interrupted => AgentProgress::Interrupted,
                ApprovalOutcome::Cancelled => AgentProgress::Cancelled,
            };
            progress.send(terminal).await;
        });
        let mut stream = AgentStreamHandle::new(driver, receiver, control);

        block_on(async {
            assert!(matches!(
                stream.next().await,
                Some(AgentProgress::ApprovalRequired { .. })
            ));
            stream
                .resolve_approval(ToolCallId::new(0), ApprovalDecision::Approved)
                .expect("the visible approval is active");
            assert_eq!(
                stream.next().await,
                Some(AgentProgress::Ended {
                    final_message: "approved".to_owned(),
                })
            );
            assert_eq!(stream.next().await, None);
        });
    }

    #[test]
    fn mismatched_approval_does_not_consume_the_waiting_request() {
        let control = AgentStreamHandle::control();
        control.begin_approval(ToolCallId::new(0));

        assert_eq!(
            control.resolve_approval(ToolCallId::new(1), ApprovalDecision::Approved),
            Err(AgentApprovalError::ToolCallMismatch {
                expected: ToolCallId::new(0),
                received: ToolCallId::new(1),
            })
        );
        control
            .resolve_approval(ToolCallId::new(0), ApprovalDecision::Approved)
            .expect("the original approval remains active");
        assert!(matches!(
            block_on(control.approval()),
            ApprovalOutcome::Decision(ApprovalDecision::Approved)
        ));
    }
}

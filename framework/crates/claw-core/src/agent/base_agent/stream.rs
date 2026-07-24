use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "cache_profile")]
use claw_api::ProviderUsage;
use claw_api::ToolCall;
use claw_memory::TurnError;
use claw_tool::{ToolDetachHandle, ToolOutput};
use claw_utils::stream::StreamPart;
use futures_core::Stream;
use futures_lite::future;

use crate::session::Message;

use super::iteration_loop::{IterationId, IterationLoopError, ToolCallId};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AgentSubmitError {
    #[error("cannot submit a message while the agent is running")]
    Running,
    #[error(transparent)]
    Transcript(#[from] TurnError),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentApprovalError {
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
pub enum AgentError {
    #[error(transparent)]
    Iteration(#[from] IterationLoopError),
    #[error(transparent)]
    Transcript(#[from] TurnError),
    #[error("multiple task effects were emitted in one tool round: {count}")]
    ConflictingEffects { count: usize },
    #[error("LLM assistant message cannot be reconstructed from streamed deltas")]
    MalformedAssistantMessage,
    #[error("agent run-state invariant violated")]
    StateInvariant,
}

/// One event produced by the single-task BaseAgent execution core.
pub(crate) enum BaseAgentEvent {
    Iteration(StreamPart<AgentIterationEvent>),
    Detached(ToolDetachHandle),
    InputRequired(AgentInputRequest),
    Finished(AgentOutcome),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentIterationEvent {
    Started(IterationId),
    Reasoning(StreamPart<String>),
    Output(StreamPart<String>),
    #[cfg(feature = "cache_profile")]
    Usage(ProviderUsage),
    ToolResult(StreamPart<(ToolCall, ToolOutput)>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentInputRequest {
    Approval {
        tool_call_id: ToolCallId,
        tool_call: ToolCall,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentOutcome {
    Completed(AgentCompletion),
    Interrupted,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentCompletion {
    /// A model response already exposed through `Output(_)` events.
    Streamed(String),
    /// A final message synthesized by an Agent effect and not streamed before.
    Synthesized(String),
}

#[derive(Clone)]
pub(super) struct RunControl {
    inner: Rc<RunSignals>,
}

struct RunSignals {
    interrupt: Cell<bool>,
    cancel: AtomicBool,
    approval: RefCell<ApprovalState>,
    continuations: RefCell<VecDeque<Message>>,
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

pub(super) enum RunStop {
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
                continuations: RefCell::new(VecDeque::new()),
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

    pub(super) fn take_continuations(&self) -> VecDeque<Message> {
        std::mem::take(&mut *self.inner.continuations.borrow_mut())
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

    pub(super) async fn stop_requested(&self) -> RunStop {
        future::poll_fn(|context| {
            if self.is_cancelled() {
                return Poll::Ready(RunStop::Cancelled);
            }
            if self.take_interrupt() {
                return Poll::Ready(RunStop::Interrupted);
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

    fn continue_with(&self, message: Message) {
        self.inner.continuations.borrow_mut().push_back(message);
        self.wake();
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

/// The unique mutable capability for one submitted BaseAgent task.
pub(crate) struct BaseAgentStream<'a> {
    stream: Pin<Box<dyn Stream<Item = Result<BaseAgentEvent, AgentError>> + 'a>>,
    control: RunControl,
}

impl<'a> BaseAgentStream<'a> {
    pub(super) fn new(
        stream: impl Stream<Item = Result<BaseAgentEvent, AgentError>> + 'a,
        control: RunControl,
    ) -> Self {
        Self {
            stream: Box::pin(stream),
            control,
        }
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

    pub(crate) fn continue_with(&mut self, message: Message) {
        self.control.continue_with(message);
    }
}

impl Stream for BaseAgentStream<'_> {
    type Item = Result<BaseAgentEvent, AgentError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().stream.as_mut().poll_next(context)
    }
}

impl Drop for BaseAgentStream<'_> {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use crate::agent::IterationId;
    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;

    use super::*;

    #[test]
    fn progress_is_a_real_poll_boundary() {
        let control = BaseAgentStream::control();
        let phase = Rc::new(Cell::new(0));
        let producer_phase = Rc::clone(&phase);
        let progress = async_stream::stream! {
            yield Ok(BaseAgentEvent::Iteration(StreamPart::Delta(
                AgentIterationEvent::Started(IterationId::new(0)),
            )));
            future::yield_now().await;
            producer_phase.set(1);
            yield Ok(BaseAgentEvent::Finished(AgentOutcome::Cancelled));
            producer_phase.set(2);
        };
        let mut stream = BaseAgentStream::new(progress, control);

        block_on(async {
            assert!(matches!(
                stream.next().await,
                Some(Ok(BaseAgentEvent::Iteration(StreamPart::Delta(
                    AgentIterationEvent::Started(iteration),
                ))))
                if iteration == IterationId::new(0)
            ));
            assert_eq!(phase.get(), 0, "producer remains parked at the boundary");

            let mut next = Box::pin(stream.next());
            assert!(future::poll_once(next.as_mut()).await.is_none());
            assert_eq!(phase.get(), 0, "yield_now ends the current poll");

            assert!(matches!(
                next.await,
                Some(Ok(BaseAgentEvent::Finished(AgentOutcome::Cancelled)))
            ));
            assert_eq!(phase.get(), 1, "the next poll resumes the producer once");

            assert!(stream.next().await.is_none());
            assert_eq!(phase.get(), 2);
        });
    }

    #[test]
    fn approval_is_resolved_through_the_stream_handle() {
        let control = BaseAgentStream::control();
        let driver_control = control.clone();
        let progress = async_stream::stream! {
            driver_control.begin_approval(ToolCallId::new(0));
            yield Ok(BaseAgentEvent::InputRequired(AgentInputRequest::Approval {
                tool_call_id: ToolCallId::new(0),
                tool_call: ToolCall::default(),
                reason: "run tool".to_owned(),
            }));
            let terminal = match driver_control.approval().await {
                ApprovalOutcome::Decision(ApprovalDecision::Approved) => {
                    AgentOutcome::Completed(AgentCompletion::Synthesized("approved".to_owned()))
                }
                ApprovalOutcome::Decision(ApprovalDecision::Rejected(_)) => {
                    AgentOutcome::Cancelled
                }
                ApprovalOutcome::Interrupted => AgentOutcome::Interrupted,
                ApprovalOutcome::Cancelled => AgentOutcome::Cancelled,
            };
            yield Ok(BaseAgentEvent::Finished(terminal));
        };
        let mut stream = BaseAgentStream::new(progress, control);

        block_on(async {
            assert!(matches!(
                stream.next().await,
                Some(Ok(BaseAgentEvent::InputRequired(
                    AgentInputRequest::Approval { .. }
                )))
            ));
            stream
                .resolve_approval(ToolCallId::new(0), ApprovalDecision::Approved)
                .expect("the visible approval is active");
            assert!(matches!(
                stream.next().await,
                Some(Ok(BaseAgentEvent::Finished(AgentOutcome::Completed(
                    AgentCompletion::Synthesized(message)
                ))))
                if message == "approved"
            ));
            assert!(stream.next().await.is_none());
        });
    }

    #[test]
    fn execution_error_is_an_err_item_followed_by_stream_end() {
        let control = BaseAgentStream::control();
        let events = async_stream::stream! {
            yield Err(AgentError::StateInvariant);
        };
        let mut stream = BaseAgentStream::new(events, control);

        block_on(async {
            assert!(matches!(
                stream.next().await,
                Some(Err(AgentError::StateInvariant))
            ));
            assert!(stream.next().await.is_none());
        });
    }

    #[test]
    fn mismatched_approval_does_not_consume_the_waiting_request() {
        let control = BaseAgentStream::control();
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

use core::pin::Pin;
use std::collections::BTreeMap;
use std::task::{Context, Poll};

use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};
use futures_core::Stream;

use crate::agent::{
    Agent, AgentApprovalError, AgentDispatchError, AgentError, AgentEvent, AgentHandle, AgentId,
    AgentStream, AgentStreamItem, ApprovalDecision, ReasoningEffort, ReasoningEffortHandle,
    ToolCallId,
};
use crate::session::Message;

pub(super) type AgentSlots<Http, Timer> = BTreeMap<AgentId, AgentSlot<Http, Timer>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InFlightLifecycle {
    Running,
    Interrupting,
    Cancelling,
    Reaping,
}

struct InFlight<Http: ClawHttp, Timer: ClawTimer> {
    stream: AgentStream<Http, Timer>,
    control: AgentHandle,
    span: tracing::Span,
    lifecycle: InFlightLifecycle,
    terminal: Option<Result<AgentEvent, AgentError>>,
}

// `Agent` is intentionally stored inline in its owning slot. Boxing it only to
// equalize enum variants adds one allocation to every resident Agent.
#[allow(clippy::large_enum_variant)]
enum Execution<Http: ClawHttp, Timer: ClawTimer> {
    Resident(Agent<Http, Timer>),
    InFlight(InFlight<Http, Timer>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentDispatch {
    Started,
    Queued,
}

pub(super) enum AgentSlotUpdate {
    Event(Result<AgentEvent, AgentError>),
    Returned,
    Reaped,
    Ignored,
}

/// The authoritative ownership record for one Agent.
///
/// A resident slot owns the Agent directly. While it is running, the slot owns
/// the AgentStream and its control capability.
pub(super) struct AgentSlot<Http: ClawHttp, Timer: ClawTimer> {
    execution: Option<Execution<Http, Timer>>,
    reasoning_effort: ReasoningEffortHandle,
}

impl<Http, Timer> AgentSlot<Http, Timer>
where
    Http: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    pub(super) fn new(agent: Agent<Http, Timer>, reasoning_effort: ReasoningEffortHandle) -> Self {
        Self {
            execution: Some(Execution::Resident(agent)),
            reasoning_effort,
        }
    }

    pub(super) fn is_in_flight(&self) -> bool {
        matches!(self.execution, Some(Execution::InFlight(_)))
    }

    fn start(&mut self, message: Message, span: tracing::Span) {
        let Some(Execution::Resident(agent)) = self.execution.take() else {
            panic!("only a resident Agent can start a stream");
        };
        let (stream, control) = agent.into_stream(message);
        self.execution = Some(Execution::InFlight(InFlight {
            stream,
            control,
            span,
            lifecycle: InFlightLifecycle::Running,
            terminal: None,
        }));
    }

    pub(super) fn dispatch(
        &mut self,
        message: Message,
        span: tracing::Span,
    ) -> Result<AgentDispatch, (Message, AgentDispatchError)> {
        match self.execution.as_mut() {
            Some(Execution::InFlight(in_flight)) => {
                let retry = message.clone();
                in_flight
                    .control
                    .dispatch(message)
                    .map(|()| AgentDispatch::Queued)
                    .map_err(|error| (retry, error))
            }
            Some(Execution::Resident(_)) => {
                self.start(message, span);
                Ok(AgentDispatch::Started)
            }
            None => Err((message, AgentDispatchError::Closed)),
        }
    }

    pub(super) fn interrupt(&mut self) {
        let Some(Execution::InFlight(in_flight)) = &mut self.execution else {
            return;
        };
        in_flight.control.interrupt();
        if in_flight.lifecycle == InFlightLifecycle::Running {
            in_flight.lifecycle = InFlightLifecycle::Interrupting;
        }
    }

    pub(super) fn cancel(&mut self) {
        let Some(Execution::InFlight(in_flight)) = &mut self.execution else {
            return;
        };
        in_flight.control.cancel();
        if in_flight.lifecycle != InFlightLifecycle::Reaping {
            in_flight.lifecycle = InFlightLifecycle::Cancelling;
        }
    }

    pub(super) fn begin_reaping(&mut self) {
        let Some(Execution::InFlight(in_flight)) = &mut self.execution else {
            return;
        };
        in_flight.control.cancel();
        in_flight.lifecycle = InFlightLifecycle::Reaping;
        in_flight.terminal = None;
    }

    pub(super) fn resolve_approval(
        &self,
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    ) -> Result<(), AgentApprovalError> {
        let Some(Execution::InFlight(in_flight)) = &self.execution else {
            return Err(AgentApprovalError::NotAwaitingApproval);
        };
        in_flight.control.resolve_approval(tool_call_id, decision)
    }

    pub(super) fn set_reasoning_effort(&self, effort: ReasoningEffort) {
        self.reasoning_effort.set(effort);
    }

    pub(super) fn poll(&mut self, context: &mut Context<'_>) -> Poll<AgentSlotUpdate> {
        let Some(Execution::InFlight(in_flight)) = self.execution.as_mut() else {
            return Poll::Pending;
        };

        let item = {
            let _entered = in_flight.span.enter();
            match Pin::new(&mut in_flight.stream).poll_next(context) {
                Poll::Ready(Some(item)) => item,
                Poll::Ready(None) => panic!("an AgentStream returns its Agent before ending"),
                Poll::Pending => return Poll::Pending,
            }
        };
        let update = match item {
            AgentStreamItem::Event(_) if in_flight.lifecycle == InFlightLifecycle::Reaping => {
                AgentSlotUpdate::Ignored
            }
            AgentStreamItem::Event(event) if event.is_err() => {
                debug_assert!(
                    in_flight.terminal.is_none(),
                    "one Agent stream has only one terminal event"
                );
                in_flight.terminal = Some(event);
                AgentSlotUpdate::Ignored
            }
            AgentStreamItem::Event(event) => AgentSlotUpdate::Event(event),
            AgentStreamItem::Returned(agent) => {
                let reaping = in_flight.lifecycle == InFlightLifecycle::Reaping;
                let terminal = in_flight.terminal.take();
                self.execution = Some(Execution::Resident(agent));
                if reaping {
                    AgentSlotUpdate::Reaped
                } else if let Some(terminal) = terminal {
                    AgentSlotUpdate::Event(terminal)
                } else {
                    AgentSlotUpdate::Returned
                }
            }
        };
        Poll::Ready(update)
    }
}

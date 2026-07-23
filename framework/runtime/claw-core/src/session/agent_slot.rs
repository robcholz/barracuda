use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};

use crate::agent::AgentId;
use crate::agent::{
    Agent, AgentApprovalError, AgentError, AgentEvent, ApprovalDecision, ReasoningEffort,
    ReasoningEffortHandle, ToolCallId,
};
use crate::scheduler::{AgentRunOutput, AgentRunOutputItem, AgentRunPort, RunControl, RunId};

use super::Message;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InFlightLifecycle {
    Running,
    Interrupting,
    Cancelling,
    Reaping,
}

struct InFlight {
    run: RunId,
    control: RunControl,
    lifecycle: InFlightLifecycle,
    terminal: Option<Result<AgentEvent, AgentError>>,
}

enum Execution<Http: ClawHttp, Timer: ClawTimer> {
    Resident(Agent<Http, Timer>),
    InFlight(InFlight),
}

pub(super) enum AgentSlotUpdate {
    Event(Result<AgentEvent, AgentError>),
    Returned,
    Reaped,
    Ignored,
}

/// The authoritative ownership record for one Agent.
///
/// A resident slot owns the Agent. While the global Scheduler polls that
/// Agent, the slot retains only the checkout epoch and its control capability.
pub(super) struct AgentSlot<Http: ClawHttp, Timer: ClawTimer> {
    id: AgentId,
    execution: Option<Execution<Http, Timer>>,
    reasoning_effort: ReasoningEffortHandle,
}

impl<Http, Timer> AgentSlot<Http, Timer>
where
    Http: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    pub(super) fn new(
        id: AgentId,
        agent: Agent<Http, Timer>,
        reasoning_effort: ReasoningEffortHandle,
    ) -> Self {
        Self {
            id,
            execution: Some(Execution::Resident(agent)),
            reasoning_effort,
        }
    }

    pub(super) fn id(&self) -> AgentId {
        self.id
    }

    pub(super) fn is_in_flight(&self) -> bool {
        matches!(self.execution, Some(Execution::InFlight(_)))
    }

    pub(super) fn start(
        &mut self,
        message: Message,
        runs: &AgentRunPort<Http, Timer>,
        span: tracing::Span,
    ) {
        let Some(Execution::Resident(agent)) = self.execution.take() else {
            panic!("only a resident Agent can start a run");
        };
        let scheduled = runs.submit(self.id, agent, message, span);
        self.execution = Some(Execution::InFlight(InFlight {
            run: scheduled.run,
            control: scheduled.control,
            lifecycle: InFlightLifecycle::Running,
            terminal: None,
        }));
    }

    pub(super) fn dispatch(
        &mut self,
        message: Message,
        runs: &AgentRunPort<Http, Timer>,
        span: tracing::Span,
    ) -> Result<(), Message> {
        match self.execution.as_mut() {
            Some(Execution::InFlight(in_flight)) => {
                let retry = message.clone();
                in_flight.control.dispatch(message).map_err(|_| retry)
            }
            Some(Execution::Resident(_)) => {
                self.start(message, runs, span);
                Ok(())
            }
            None => Err(message),
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

    pub(super) fn accept_output(&mut self, output: AgentRunOutput<Http, Timer>) -> AgentSlotUpdate {
        if output.agent != self.id {
            return AgentSlotUpdate::Ignored;
        }
        let Some(Execution::InFlight(in_flight)) = self.execution.as_mut() else {
            return AgentSlotUpdate::Ignored;
        };
        if output.run != in_flight.run {
            tracing::warn!(
                name: "stale_agent_run_output",
                agent = %self.id,
                expected = ?in_flight.run,
                received = ?output.run,
            );
            return AgentSlotUpdate::Ignored;
        }

        match output.item {
            AgentRunOutputItem::Event(_) if in_flight.lifecycle == InFlightLifecycle::Reaping => {
                AgentSlotUpdate::Ignored
            }
            AgentRunOutputItem::Event(event) if event.is_err() => {
                debug_assert!(
                    in_flight.terminal.is_none(),
                    "one Agent run has only one terminal event"
                );
                in_flight.terminal = Some(event);
                AgentSlotUpdate::Ignored
            }
            AgentRunOutputItem::Event(event) => AgentSlotUpdate::Event(event),
            AgentRunOutputItem::Returned(agent) => {
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
        }
    }
}

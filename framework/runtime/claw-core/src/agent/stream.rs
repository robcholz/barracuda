use core::pin::Pin;
use core::task::{Context, Poll};
use std::cell::RefCell;
use std::rc::Rc;

use async_channel::{Receiver, Sender};
use claw_api::ToolCall;
use futures_core::Stream;

use super::base_agent::{
    AgentApprovalError, AgentInputRequest, AgentIterationEvent, AgentOutcome, ApprovalDecision,
    ToolCallId,
};
use super::AgentError;
use crate::session::Message;

/// Why the long-lived Agent wrapper opened one BaseAgent task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentTurnOrigin {
    Message,
    ToolCall { call: ToolCall },
}

/// One event from the long-lived Agent wrapper.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentEvent {
    TurnStarted { origin: AgentTurnOrigin },
    Iteration(claw_utils::stream::StreamPart<AgentIterationEvent>),
    InputRequired(AgentInputRequest),
    TurnEnded { outcome: AgentOutcome },
}

pub(super) enum AgentCommand {
    Submit(Message),
    Interrupt,
    Cancel,
    ResolveApproval {
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    },
}

/// Cloneable control capability for one checked-out Agent.
#[derive(Clone)]
pub(super) struct AgentControl {
    commands: Sender<AgentCommand>,
    awaiting_approval: Rc<RefCell<Option<ToolCallId>>>,
}

impl AgentControl {
    fn submit(&self, message: Message) -> bool {
        self.commands
            .try_send(AgentCommand::Submit(message))
            .is_ok()
    }

    fn interrupt(&self) {
        let _ = self.commands.try_send(AgentCommand::Interrupt);
    }

    fn cancel(&self) {
        let _ = self.commands.try_send(AgentCommand::Cancel);
    }

    fn resolve_approval(
        &self,
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    ) -> Result<(), AgentApprovalError> {
        let mut awaiting = self.awaiting_approval.borrow_mut();
        let Some(expected) = *awaiting else {
            return Err(AgentApprovalError::NotAwaitingApproval);
        };
        if expected != tool_call_id {
            return Err(AgentApprovalError::ToolCallMismatch {
                expected,
                received: tool_call_id,
            });
        }
        awaiting.take();
        drop(awaiting);
        self.commands
            .try_send(AgentCommand::ResolveApproval {
                tool_call_id,
                decision,
            })
            .map_err(|_| AgentApprovalError::NotAwaitingApproval)
    }
}

/// Borrowing stream and control surface for one physical Agent checkout.
pub(crate) struct AgentStream<'a> {
    stream: Pin<Box<dyn Stream<Item = Result<AgentEvent, AgentError>> + 'a>>,
    control: AgentControl,
}

impl<'a> AgentStream<'a> {
    pub(super) fn channel() -> (
        AgentControl,
        Receiver<AgentCommand>,
        Rc<RefCell<Option<ToolCallId>>>,
    ) {
        let (commands, receiver) = async_channel::unbounded();
        let awaiting_approval = Rc::new(RefCell::new(None));
        (
            AgentControl {
                commands,
                awaiting_approval: Rc::clone(&awaiting_approval),
            },
            receiver,
            awaiting_approval,
        )
    }

    pub(super) fn new(
        stream: impl Stream<Item = Result<AgentEvent, AgentError>> + 'a,
        control: AgentControl,
    ) -> Self {
        Self {
            stream: Box::pin(stream),
            control,
        }
    }

    pub(crate) fn submit(&mut self, message: Message) -> bool {
        self.control.submit(message)
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
}

impl Stream for AgentStream<'_> {
    type Item = Result<AgentEvent, AgentError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().stream.as_mut().poll_next(context)
    }
}

impl Drop for AgentStream<'_> {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

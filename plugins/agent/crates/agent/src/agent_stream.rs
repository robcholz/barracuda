use alloc::{boxed::Box, rc::Rc};
use core::cell::{Cell, RefCell};
use core::pin::Pin;
use core::task::{Context, Poll};

use async_channel::{Receiver, Sender};
use barracuda_model_api::ToolCall;
use futures_core::Stream;
use http_client::embedded_nal_async::{Dns, TcpConnect};

use crate::engine::AgentError;
use crate::engine::{
    AgentApprovalError, AgentInputRequest, AgentIterationEvent, AgentOutcome, ApprovalDecision,
    ToolCallId,
};
use crate::Agent;
use crate::Message;

/// Why the long-lived Agent wrapper opened one AgentEngine task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentTurnOrigin {
    Message,
    ToolCall { call: ToolCall },
}

/// One event from the long-lived Agent wrapper.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    TurnStarted { origin: AgentTurnOrigin },
    Iteration(barracuda_runtime_utils::stream::StreamPart<AgentIterationEvent>),
    InputRequired(AgentInputRequest),
    TurnEnded { outcome: AgentOutcome },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentDispatchError {
    Busy,
    Closed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentActivity {
    Running,
    Idle,
    Closed,
}

pub(super) enum AgentCommand {
    Dispatch(Message),
    Interrupt,
    Cancel,
    ResolveApproval {
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    },
}

/// Control capability for one checked-out Agent.
pub struct AgentHandle {
    commands: Sender<AgentCommand>,
    activity: Rc<Cell<AgentActivity>>,
    awaiting_approval: Rc<RefCell<Option<ToolCallId>>>,
}

type AgentChannel = (
    AgentHandle,
    Receiver<AgentCommand>,
    Rc<Cell<AgentActivity>>,
    Rc<RefCell<Option<ToolCallId>>>,
);

impl AgentHandle {
    pub(super) fn channel() -> AgentChannel {
        let (commands, receiver) = async_channel::unbounded();
        let activity = Rc::new(Cell::new(AgentActivity::Running));
        let awaiting_approval = Rc::new(RefCell::new(None));
        (
            Self {
                commands,
                activity: Rc::clone(&activity),
                awaiting_approval: Rc::clone(&awaiting_approval),
            },
            receiver,
            activity,
            awaiting_approval,
        )
    }

    pub fn dispatch(&self, message: Message) -> Result<(), AgentDispatchError> {
        match self.activity.get() {
            AgentActivity::Running => return Err(AgentDispatchError::Busy),
            AgentActivity::Closed => return Err(AgentDispatchError::Closed),
            AgentActivity::Idle => self.activity.set(AgentActivity::Running),
        }
        if self
            .commands
            .try_send(AgentCommand::Dispatch(message))
            .is_err()
        {
            self.activity.set(AgentActivity::Closed);
            return Err(AgentDispatchError::Closed);
        }
        Ok(())
    }

    pub fn interrupt(&self) {
        if self.activity.get() == AgentActivity::Running {
            let _ = self.commands.try_send(AgentCommand::Interrupt);
        }
    }

    pub fn cancel(&self) {
        if self.activity.replace(AgentActivity::Closed) == AgentActivity::Closed {
            return;
        }
        let _ = self.commands.try_send(AgentCommand::Cancel);
    }

    pub fn resolve_approval(
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

/// One item produced by an owned physical Agent checkout.
///
/// The final item returns the Agent to its authoritative slot.
#[allow(clippy::large_enum_variant)]
pub enum AgentStreamItem<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    Event(Result<AgentEvent, AgentError>),
    Returned(Agent<Tcp, Resolver>),
}

/// Owned event stream for one physical Agent checkout.
pub struct AgentStream<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    stream: Pin<Box<dyn Stream<Item = AgentStreamItem<Tcp, Resolver>>>>,
}

impl<Tcp, Resolver> AgentStream<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    pub(super) fn new(
        stream: impl Stream<Item = AgentStreamItem<Tcp, Resolver>> + 'static,
    ) -> Self {
        Self {
            stream: Box::pin(stream),
        }
    }
}

impl<Tcp, Resolver> Stream for AgentStream<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    type Item = AgentStreamItem<Tcp, Resolver>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().stream.as_mut().poll_next(context)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_accepts_exactly_one_message_while_idle() {
        let (handle, commands, activity, _) = AgentHandle::channel();

        assert_eq!(
            handle.dispatch(Message::text("busy")),
            Err(AgentDispatchError::Busy)
        );

        activity.set(AgentActivity::Idle);
        assert_eq!(handle.dispatch(Message::text("next")), Ok(()));
        assert_eq!(
            handle.dispatch(Message::text("queued")),
            Err(AgentDispatchError::Busy)
        );

        let command = commands
            .try_recv()
            .expect("the accepted dispatch reaches the Agent");
        let AgentCommand::Dispatch(message) = command else {
            panic!("dispatch emits only a dispatch command");
        };
        assert_eq!(message.as_str(), "next");
    }

    #[test]
    fn cancel_closes_dispatch() {
        let (handle, _, activity, _) = AgentHandle::channel();

        activity.set(AgentActivity::Idle);
        handle.cancel();

        assert_eq!(
            handle.dispatch(Message::text("closed")),
            Err(AgentDispatchError::Closed)
        );
    }
}

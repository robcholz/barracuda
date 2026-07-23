use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::cell::RefCell;
use std::rc::Rc;

use async_channel::{Receiver, Sender};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};
use futures_core::Stream;
use futures_lite::{future, StreamExt as _};
use tracing::Instrument as _;

use crate::agent::{
    Agent, AgentApprovalError, AgentDispatchError, AgentError, AgentEvent, AgentHandle,
    AgentInputRequest, ApprovalDecision, ToolCallId,
};
use crate::session::Message;

pub(super) enum AgentRunItem {
    Event(Result<AgentEvent, AgentError>),
    Returned,
}

enum RunCommand {
    Dispatch(Message),
    Interrupt,
    Cancel,
    ResolveApproval {
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunActivity {
    Running,
    Idle,
    Closed,
}

#[derive(Clone)]
pub(crate) struct AgentRunControl {
    commands: Sender<RunCommand>,
    activity: Rc<Cell<RunActivity>>,
    awaiting_approval: Rc<RefCell<Option<ToolCallId>>>,
}

impl AgentRunControl {
    pub(crate) fn dispatch(&self, message: Message) -> Result<(), AgentDispatchError> {
        match self.activity.get() {
            RunActivity::Running => return Err(AgentDispatchError::Busy),
            RunActivity::Closed => return Err(AgentDispatchError::Closed),
            RunActivity::Idle => self.activity.set(RunActivity::Running),
        }
        if self
            .commands
            .try_send(RunCommand::Dispatch(message))
            .is_err()
        {
            self.activity.set(RunActivity::Closed);
            return Err(AgentDispatchError::Closed);
        }
        Ok(())
    }

    pub(crate) fn interrupt(&self) {
        if self.activity.get() == RunActivity::Running {
            let _ = self.commands.try_send(RunCommand::Interrupt);
        }
    }

    pub(crate) fn cancel(&self) {
        if self.activity.replace(RunActivity::Closed) == RunActivity::Closed {
            return;
        }
        let _ = self.commands.try_send(RunCommand::Cancel);
    }

    pub(crate) fn resolve_approval(
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
            .try_send(RunCommand::ResolveApproval {
                tool_call_id,
                decision,
            })
            .map_err(|_| AgentApprovalError::NotAwaitingApproval)
    }
}

struct ProgressEnvelope {
    item: Result<AgentEvent, AgentError>,
    resume: Sender<()>,
}

type AgentRunFuture<Http, Timer> = Pin<Box<dyn Future<Output = Agent<Http, Timer>>>>;

/// Scheduler-private polling adapter around the self-contained Agent stream.
pub(super) struct AgentRun<Http: ClawHttp, Timer: ClawTimer> {
    control: AgentRunControl,
    progress: Pin<Box<Receiver<ProgressEnvelope>>>,
    resume: Option<Sender<()>>,
    future: Option<AgentRunFuture<Http, Timer>>,
    agent: Option<Agent<Http, Timer>>,
    returned: bool,
}

impl<Http: ClawHttp, Timer: ClawTimer> Unpin for AgentRun<Http, Timer> {}

impl<Http, Timer> AgentRun<Http, Timer>
where
    Http: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    pub(super) fn start(agent: Agent<Http, Timer>, message: Message, span: tracing::Span) -> Self {
        let (progress_sender, progress_receiver) = async_channel::bounded(1);
        let (command_sender, command_receiver) = async_channel::unbounded();
        let activity = Rc::new(Cell::new(RunActivity::Running));
        let awaiting_approval = Rc::new(RefCell::new(None));
        let control = AgentRunControl {
            commands: command_sender,
            activity: Rc::clone(&activity),
            awaiting_approval: Rc::clone(&awaiting_approval),
        };
        let future = Box::pin(
            drive_agent(
                agent,
                message,
                progress_sender,
                command_receiver,
                activity,
                awaiting_approval,
            )
            .instrument(span),
        );
        Self {
            control,
            progress: Box::pin(progress_receiver),
            resume: None,
            future: Some(future),
            agent: None,
            returned: false,
        }
    }

    pub(super) fn control(&self) -> AgentRunControl {
        self.control.clone()
    }

    pub(super) fn poll_event(&mut self, context: &mut Context<'_>) -> Poll<AgentRunItem> {
        Pin::new(self)
            .poll_next(context)
            .map(|event| event.expect("AgentRun ends after returning its Agent"))
    }

    pub(super) fn take_completed_agent(&mut self) -> Option<Agent<Http, Timer>> {
        self.returned.then(|| self.agent.take()).flatten()
    }

    fn take_progress(&mut self, context: &mut Context<'_>) -> Poll<Option<AgentRunItem>> {
        match self.progress.as_mut().poll_next(context) {
            Poll::Ready(Some(envelope)) => {
                self.resume = Some(envelope.resume);
                Poll::Ready(Some(AgentRunItem::Event(envelope.item)))
            }
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }
}

impl<Http, Timer> Stream for AgentRun<Http, Timer>
where
    Http: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    type Item = AgentRunItem;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.returned {
            return Poll::Ready(None);
        }

        if let Some(resume) = this.resume.take() {
            let _ = resume.try_send(());
        }

        if let Poll::Ready(event) = this.take_progress(context) {
            return Poll::Ready(event);
        }

        if let Some(future) = this.future.as_mut() {
            if let Poll::Ready(agent) = future.as_mut().poll(context) {
                this.future = None;
                this.agent = Some(agent);
                this.returned = true;
                return Poll::Ready(Some(AgentRunItem::Returned));
            }
        }

        this.take_progress(context)
    }
}

async fn drive_agent<Http, Timer>(
    mut agent: Agent<Http, Timer>,
    message: Message,
    progress: Sender<ProgressEnvelope>,
    commands: Receiver<RunCommand>,
    activity: Rc<Cell<RunActivity>>,
    awaiting_approval: Rc<RefCell<Option<ToolCallId>>>,
) -> Agent<Http, Timer>
where
    Http: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    let (mut stream, handle): (_, AgentHandle) = agent.submit(message);

    loop {
        enum Wake {
            Event(Option<Result<AgentEvent, AgentError>>),
            Command(Option<RunCommand>),
        }

        let wake = future::or(async { Wake::Command(commands.recv().await.ok()) }, async {
            Wake::Event(stream.next().await)
        })
        .await;

        match wake {
            Wake::Command(Some(RunCommand::Dispatch(message))) => {
                if handle.dispatch(message).is_err() {
                    activity.set(RunActivity::Closed);
                    handle.cancel();
                    break;
                }
            }
            Wake::Command(Some(RunCommand::Interrupt)) => handle.interrupt(),
            Wake::Command(Some(RunCommand::Cancel)) => handle.cancel(),
            Wake::Command(Some(RunCommand::ResolveApproval {
                tool_call_id,
                decision,
            })) => {
                let _ = handle.resolve_approval(tool_call_id, decision);
            }
            Wake::Command(None) => handle.cancel(),
            Wake::Event(Some(item)) => {
                match &item {
                    Ok(AgentEvent::TurnStarted { .. }) => {
                        activity.set(RunActivity::Running);
                    }
                    Ok(AgentEvent::InputRequired(AgentInputRequest::Approval {
                        tool_call_id,
                        ..
                    })) => {
                        *awaiting_approval.borrow_mut() = Some(*tool_call_id);
                    }
                    Ok(AgentEvent::TurnEnded { .. }) => {
                        awaiting_approval.borrow_mut().take();
                        activity.set(RunActivity::Idle);
                    }
                    Err(_) => {
                        awaiting_approval.borrow_mut().take();
                        activity.set(RunActivity::Closed);
                    }
                    _ => {}
                }
                let terminal = item.is_err();
                let (resume, resumed) = async_channel::bounded(1);
                if progress
                    .send(ProgressEnvelope { item, resume })
                    .await
                    .is_err()
                {
                    handle.cancel();
                    break;
                }
                let _ = resumed.recv().await;
                if terminal {
                    let _ = stream.next().await;
                    break;
                }
            }
            Wake::Event(None) => {
                activity.set(RunActivity::Closed);
                break;
            }
        }
    }

    drop(stream);
    activity.set(RunActivity::Closed);
    agent
}

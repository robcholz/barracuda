//! Process-global physical Agent execution and fair scheduling.

mod run;

use core::cell::{Cell, RefCell};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::collections::VecDeque;
use std::rc::Rc;

use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};
use futures_core::Stream;

use crate::agent::AgentId;
use crate::agent::{Agent, AgentError, AgentEvent};
use crate::session::Message;

use run::{AgentRun, AgentRunControl, AgentRunItem};

/// One checkout epoch. A late event from an older run cannot mutate a newer
/// resident Agent in the same slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunId(u64);

/// Control capability for one checked-out Agent.
pub(crate) use run::AgentRunControl as RunControl;

/// A scheduler event routed back to the logical Agent owner.
pub(crate) struct AgentRunOutput<Http: ClawHttp, Timer: ClawTimer> {
    pub(crate) agent: AgentId,
    pub(crate) run: RunId,
    pub(crate) item: AgentRunOutputItem<Http, Timer>,
}

pub(crate) enum AgentRunOutputItem<Http: ClawHttp, Timer: ClawTimer> {
    Event(Result<AgentEvent, AgentError>),
    Returned(Agent<Http, Timer>),
}

struct RouteState<Http: ClawHttp, Timer: ClawTimer> {
    outputs: VecDeque<AgentRunOutput<Http, Timer>>,
    waiter: Option<Waker>,
}

/// Opaque scheduler return route. It carries no Session or Multiagent identity.
pub(crate) struct AgentRunRoute<Http: ClawHttp, Timer: ClawTimer> {
    state: Rc<RefCell<RouteState<Http, Timer>>>,
}

impl<Http: ClawHttp, Timer: ClawTimer> Clone for AgentRunRoute<Http, Timer> {
    fn clone(&self) -> Self {
        Self {
            state: Rc::clone(&self.state),
        }
    }
}

pub(crate) struct AgentRunReceiver<Http: ClawHttp, Timer: ClawTimer> {
    state: Rc<RefCell<RouteState<Http, Timer>>>,
}

pub(crate) fn agent_run_route<Http, Timer>(
) -> (AgentRunRoute<Http, Timer>, AgentRunReceiver<Http, Timer>)
where
    Http: ClawHttp,
    Timer: ClawTimer,
{
    let state = Rc::new(RefCell::new(RouteState {
        outputs: VecDeque::new(),
        waiter: None,
    }));
    (
        AgentRunRoute {
            state: Rc::clone(&state),
        },
        AgentRunReceiver { state },
    )
}

impl<Http: ClawHttp, Timer: ClawTimer> AgentRunRoute<Http, Timer> {
    fn send(&self, output: AgentRunOutput<Http, Timer>) {
        let waiter = {
            let mut state = self.state.borrow_mut();
            state.outputs.push_back(output);
            state.waiter.take()
        };
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }
}

impl<Http: ClawHttp, Timer: ClawTimer> Stream for AgentRunReceiver<Http, Timer> {
    type Item = AgentRunOutput<Http, Timer>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut state = self.state.borrow_mut();
        if let Some(output) = state.outputs.pop_front() {
            return Poll::Ready(Some(output));
        }
        if state
            .waiter
            .as_ref()
            .is_none_or(|waiter| !waiter.will_wake(context.waker()))
        {
            state.waiter = Some(context.waker().clone());
        }
        Poll::Pending
    }
}

struct Submission<Http: ClawHttp, Timer: ClawTimer> {
    agent: AgentId,
    run: RunId,
    route: AgentRunRoute<Http, Timer>,
    task: AgentRun<Http, Timer>,
}

struct SchedulerInbox<Http: ClawHttp, Timer: ClawTimer> {
    submissions: RefCell<VecDeque<Submission<Http, Timer>>>,
    next_run: Cell<u64>,
    waiter: RefCell<Option<Waker>>,
}

/// Single-thread-local submission capability. It transfers an Agent into the
/// scheduler but never polls it.
pub(crate) struct AgentRunSchedulerHandle<Http: ClawHttp, Timer: ClawTimer> {
    inbox: Rc<SchedulerInbox<Http, Timer>>,
}

impl<Http: ClawHttp, Timer: ClawTimer> Clone for AgentRunSchedulerHandle<Http, Timer> {
    fn clone(&self) -> Self {
        Self {
            inbox: Rc::clone(&self.inbox),
        }
    }
}

pub(crate) struct ScheduledAgent {
    pub(crate) run: RunId,
    pub(crate) control: AgentRunControl,
}

impl<Http, Timer> AgentRunSchedulerHandle<Http, Timer>
where
    Http: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    pub(crate) fn submit(
        &self,
        agent_id: AgentId,
        agent: Agent<Http, Timer>,
        message: Message,
        route: AgentRunRoute<Http, Timer>,
        span: tracing::Span,
    ) -> ScheduledAgent {
        let run_id = RunId(self.inbox.next_run.get());
        self.inbox
            .next_run
            .set(run_id.0.checked_add(1).expect("RunId space exhausted"));
        let task = AgentRun::start(agent, message, span);
        let control = task.control();
        self.inbox.submissions.borrow_mut().push_back(Submission {
            agent: agent_id,
            run: run_id,
            route,
            task,
        });
        if let Some(waiter) = self.inbox.waiter.borrow_mut().take() {
            waiter.wake();
        }
        ScheduledAgent {
            run: run_id,
            control,
        }
    }
}

/// The only component that polls checked-out Agents.
pub(crate) struct AgentRunScheduler<Http: ClawHttp, Timer: ClawTimer> {
    inbox: Rc<SchedulerInbox<Http, Timer>>,
    active: VecDeque<Submission<Http, Timer>>,
}

impl<Http: ClawHttp, Timer: ClawTimer> AgentRunScheduler<Http, Timer> {
    pub(crate) fn new() -> (Self, AgentRunSchedulerHandle<Http, Timer>) {
        let inbox = Rc::new(SchedulerInbox {
            submissions: RefCell::new(VecDeque::new()),
            next_run: Cell::new(1),
            waiter: RefCell::new(None),
        });
        (
            Self {
                inbox: Rc::clone(&inbox),
                active: VecDeque::new(),
            },
            AgentRunSchedulerHandle { inbox },
        )
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.active.is_empty() && self.inbox.submissions.borrow().is_empty()
    }

    fn accept_submissions(&mut self) {
        self.active
            .extend(self.inbox.submissions.borrow_mut().drain(..));
    }
}

impl<Http, Timer> Stream for AgentRunScheduler<Http, Timer>
where
    Http: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    type Item = ();

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.accept_submissions();

        let sweep_len = this.active.len();
        for _ in 0..sweep_len {
            let mut scheduled = this
                .active
                .pop_front()
                .expect("fair sweep length matches the active queue");
            match scheduled.task.poll_event(context) {
                Poll::Ready(AgentRunItem::Event(event)) => {
                    scheduled.route.send(AgentRunOutput {
                        agent: scheduled.agent,
                        run: scheduled.run,
                        item: AgentRunOutputItem::Event(event),
                    });
                    this.active.push_back(scheduled);
                    return Poll::Ready(Some(()));
                }
                Poll::Ready(AgentRunItem::Returned) => {
                    let agent = scheduled
                        .task
                        .take_completed_agent()
                        .expect("completed AgentRun returns its Agent once");
                    scheduled.route.send(AgentRunOutput {
                        agent: scheduled.agent,
                        run: scheduled.run,
                        item: AgentRunOutputItem::Returned(agent),
                    });
                    return Poll::Ready(Some(()));
                }
                Poll::Pending => this.active.push_back(scheduled),
            }
        }

        let mut waiter = this.inbox.waiter.borrow_mut();
        if waiter
            .as_ref()
            .is_none_or(|waiter| !waiter.will_wake(context.waker()))
        {
            *waiter = Some(context.waker().clone());
        }
        Poll::Pending
    }
}

impl<Http: ClawHttp, Timer: ClawTimer> Unpin for AgentRunScheduler<Http, Timer> {}

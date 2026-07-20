use core::cell::RefCell;
use core::future::{poll_fn, Future};
use core::pin::Pin;
use core::task::{Context, Poll};
use std::rc::Rc;

use crate::protocol::{EventSink, TrackedToolCall};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};

use super::{BaseAgent, TickOutcome};

#[derive(Clone, Debug)]
pub(crate) enum AgentEvent {
    ToolStarted(TrackedToolCall),
    TickFinished(TickOutcome),
}

type AgentTickFuture<Http, Timer> =
    Pin<Box<dyn Future<Output = (BaseAgent<Http, Timer>, TickOutcome)>>>;

/// One owned agent tick that yields [`AgentEvent`] values to its caller.
pub(crate) struct AgentRun<Http: ClawHttp, Timer: ClawTimer> {
    boundary: AgentEventBoundary,
    future: Option<AgentTickFuture<Http, Timer>>,
    finished_agent: Option<BaseAgent<Http, Timer>>,
}

impl<Http, Timer> AgentRun<Http, Timer>
where
    Http: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    fn start(mut agent: BaseAgent<Http, Timer>, events: EventSink) -> Self {
        let boundary = AgentEventBoundary::default();
        let tick_boundary = boundary.clone();
        let future = Box::pin(async move {
            let outcome = agent.tick(&events, &tick_boundary).await;
            (agent, outcome)
        });
        Self {
            boundary,
            future: Some(future),
            finished_agent: None,
        }
    }

    pub(crate) fn poll_event(&mut self, context: &mut Context<'_>) -> Poll<AgentEvent> {
        let future = self
            .future
            .as_mut()
            .expect("finished agent run must not be polled again");
        match future.as_mut().poll(context) {
            Poll::Ready((agent, outcome)) => {
                self.future = None;
                self.finished_agent = Some(agent);
                Poll::Ready(AgentEvent::TickFinished(outcome))
            }
            Poll::Pending => match self.boundary.take() {
                Some(call) => Poll::Ready(AgentEvent::ToolStarted(call)),
                None => Poll::Pending,
            },
        }
    }

    pub(crate) fn take_finished_agent(&mut self) -> Option<BaseAgent<Http, Timer>> {
        self.finished_agent.take()
    }
}

impl<Http, Timer> BaseAgent<Http, Timer>
where
    Http: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    pub(crate) fn run(self, events: EventSink) -> AgentRun<Http, Timer> {
        AgentRun::start(self, events)
    }
}

/// Private yield point shared by one running agent tick and its owner.
///
/// A tool call publishes itself on the first poll and may continue only after
/// the owner has observed that event and polls the tick again.
#[derive(Clone, Default)]
pub(crate) struct AgentEventBoundary {
    pending: Rc<RefCell<Option<TrackedToolCall>>>,
}

impl AgentEventBoundary {
    pub(crate) fn take(&self) -> Option<TrackedToolCall> {
        self.pending.borrow_mut().take()
    }

    pub(crate) async fn tool_started(&self, call: TrackedToolCall) {
        let mut call = Some(call);
        poll_fn(|context| {
            let Some(call) = call.take() else {
                return Poll::Ready(());
            };
            let replaced = self.pending.borrow_mut().replace(call);
            debug_assert!(replaced.is_none(), "agent event was not consumed");
            context.waker().wake_by_ref();
            Poll::Pending
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use core::future::Future as _;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use serde_json::json;

    use super::AgentEventBoundary;
    use crate::protocol::TrackedToolCall;

    #[test]
    fn tool_start_is_observable_before_execution_resumes() {
        let boundary = AgentEventBoundary::default();
        let call = TrackedToolCall::new("profile_read", json!({"document":"user"}));
        let mut started = pin!(boundary.tool_started(call.clone()));
        let mut context = Context::from_waker(Waker::noop());

        assert!(matches!(started.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(boundary.take(), Some(call));
        assert!(matches!(
            started.as_mut().poll(&mut context),
            Poll::Ready(())
        ));
    }
}

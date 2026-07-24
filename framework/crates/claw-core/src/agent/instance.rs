use core::pin::Pin;
use core::task::{Context, Poll};
use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::rc::Rc;

use async_channel::{Receiver, TryRecvError};
use claw_api::ToolCall;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};
use claw_persistence::DurableState;
use claw_tool::{ToolDetachHandle, ToolInvocation, ToolOutput};
use futures_core::Stream;
use futures_lite::{future, StreamExt as _};

use super::base_agent::{
    AgentInputRequest, AgentOutcome, AgentSubmitError, BaseAgent, BaseAgentEvent,
};
use super::stream::{
    AgentActivity, AgentCommand, AgentEvent, AgentHandle, AgentStream, AgentStreamItem,
    AgentTurnOrigin,
};
use super::{AgentError, AgentIterationEvent, BaseAgentState};
use crate::session::Message;

#[derive(Clone)]
struct DetachedCompletion {
    call: ToolCall,
    output: ToolOutput,
}

impl DetachedCompletion {
    fn from_tool((invocation, output): (ToolInvocation, ToolOutput)) -> Self {
        Self {
            call: ToolCall {
                id: invocation.id().unwrap_or_default().to_owned(),
                name: invocation.name().to_owned(),
                arguments_json: invocation.arguments_json().to_owned(),
            },
            output,
        }
    }
}

/// Runtime-only state that disappears when this Agent is dropped or restarted.
struct AgentEphemeralState {
    inflight_detached_toolcalls: VecDeque<ToolDetachHandle>,
    ready_detached_toolcalls: VecDeque<DetachedCompletion>,
}

impl AgentEphemeralState {
    fn new() -> Self {
        Self {
            inflight_detached_toolcalls: VecDeque::new(),
            ready_detached_toolcalls: VecDeque::new(),
        }
    }

    fn push(&mut self, handle: ToolDetachHandle) {
        self.inflight_detached_toolcalls.push_back(handle);
    }

    fn poll_completion(&mut self, context: &mut Context<'_>) -> Poll<DetachedCompletion> {
        let count = self.inflight_detached_toolcalls.len();
        for _ in 0..count {
            let Some(mut handle) = self.inflight_detached_toolcalls.pop_front() else {
                break;
            };
            match Pin::new(&mut handle).poll_next(context) {
                Poll::Ready(Some(completion)) => {
                    self.inflight_detached_toolcalls.push_back(handle);
                    return Poll::Ready(DetachedCompletion::from_tool(completion));
                }
                Poll::Ready(None) => {}
                Poll::Pending => self.inflight_detached_toolcalls.push_back(handle),
            }
        }
        Poll::Pending
    }

    fn has_inflight_detached_toolcalls(&self) -> bool {
        !self.inflight_detached_toolcalls.is_empty()
    }

    fn clear(&mut self) {
        self.inflight_detached_toolcalls.clear();
        self.ready_detached_toolcalls.clear();
    }

    async fn next_turn(
        &mut self,
        commands: &Receiver<AgentCommand>,
        activity: &Cell<AgentActivity>,
    ) -> Option<PendingTurn> {
        loop {
            match commands.try_recv() {
                Ok(AgentCommand::Dispatch(message)) => {
                    return Some(PendingTurn::message(message));
                }
                Ok(AgentCommand::Interrupt) | Ok(AgentCommand::ResolveApproval { .. }) => continue,
                Ok(AgentCommand::Cancel) | Err(TryRecvError::Closed) => {
                    self.clear();
                    return None;
                }
                Err(TryRecvError::Empty) => {}
            }

            if !self.ready_detached_toolcalls.is_empty() && activity.get() == AgentActivity::Idle {
                activity.set(AgentActivity::Running);
                return detached_turn(&mut self.ready_detached_toolcalls);
            }
            if !self.has_inflight_detached_toolcalls() {
                return None;
            }

            enum Wake {
                Command(Option<AgentCommand>),
                Detached(DetachedCompletion),
            }
            match future::or(async { Wake::Command(commands.recv().await.ok()) }, async {
                Wake::Detached(future::poll_fn(|context| self.poll_completion(context)).await)
            })
            .await
            {
                Wake::Command(Some(AgentCommand::Dispatch(message))) => {
                    return Some(PendingTurn::message(message));
                }
                Wake::Command(Some(AgentCommand::Interrupt))
                | Wake::Command(Some(AgentCommand::ResolveApproval { .. })) => {}
                Wake::Command(Some(AgentCommand::Cancel)) | Wake::Command(None) => {
                    self.clear();
                    return None;
                }
                Wake::Detached(completion) => {
                    self.ready_detached_toolcalls.push_back(completion);
                }
            }
        }
    }
}

struct PendingTurn {
    origin: AgentTurnOrigin,
    message: Message,
    applied_completions: Vec<DetachedCompletion>,
}

impl PendingTurn {
    fn message(message: Message) -> Self {
        Self {
            origin: AgentTurnOrigin::Message,
            message,
            applied_completions: Vec::new(),
        }
    }
}

/// One long-lived Agent instance around the single-task [`BaseAgent`] core.
pub(crate) struct Agent<H: ClawHttp, Timer: ClawTimer> {
    base: BaseAgent<H, Timer>,
    ephemeral: AgentEphemeralState,
}

impl<H, Timer> Agent<H, Timer>
where
    H: ClawHttp + StreamingHttp,
    Timer: ClawTimer,
{
    pub(super) fn new(base: BaseAgent<H, Timer>) -> Self {
        Self {
            base,
            ephemeral: AgentEphemeralState::new(),
        }
    }

    pub(crate) fn into_stream(self, message: Message) -> (AgentStream<H, Timer>, AgentHandle)
    where
        H: 'static,
        Timer: 'static,
    {
        let (handle, commands, activity, awaiting_approval) = AgentHandle::channel();
        let stream =
            OwnedAgentGuard::new(self, activity).into_stream(message, commands, awaiting_approval);
        (AgentStream::new(stream), handle)
    }

    pub(in crate::agent) fn state(&self) -> &DurableState<BaseAgentState> {
        self.base.state()
    }
}

struct OwnedAgentGuard<H: ClawHttp, Timer: ClawTimer> {
    agent: Option<Agent<H, Timer>>,
    activity: Rc<Cell<AgentActivity>>,
}

impl<H, Timer> OwnedAgentGuard<H, Timer>
where
    H: ClawHttp + StreamingHttp + 'static,
    Timer: ClawTimer + 'static,
{
    fn new(agent: Agent<H, Timer>, activity: Rc<Cell<AgentActivity>>) -> Self {
        Self {
            agent: Some(agent),
            activity,
        }
    }

    fn into_stream(
        mut self,
        first_message: Message,
        commands: Receiver<AgentCommand>,
        awaiting_approval: Rc<std::cell::RefCell<Option<super::ToolCallId>>>,
    ) -> impl futures_core::Stream<Item = AgentStreamItem<H, Timer>> + 'static {
        async_stream::stream! {
            let activity = Rc::clone(&self.activity);
            {
                let Agent { base, ephemeral } = self
                    .agent
                    .as_mut()
                    .expect("an active Agent stream retains its Agent");
                let mut turn = PendingTurn::message(first_message);
                let mut cancel_requested = false;

                loop {
                    yield AgentStreamItem::Event(Ok(AgentEvent::TurnStarted {
                        origin: turn.origin.clone(),
                    }));

                    let mut run = match base.submit(turn.message) {
                        Ok(run) => run,
                        Err(AgentSubmitError::Transcript(error)) => {
                            yield AgentStreamItem::Event(Err(AgentError::Transcript(error)));
                            ephemeral.clear();
                            break;
                        }
                        Err(AgentSubmitError::Running) => {
                            yield AgentStreamItem::Event(Err(AgentError::StateInvariant));
                            ephemeral.clear();
                            break;
                        }
                    };
                    let mut applied_completions = turn.applied_completions;
                    let mut pending_completions = Vec::new();
                    let mut outcome = None;
                    let mut failure = None;

                    while outcome.is_none() && failure.is_none() {
                        enum ActiveWake {
                            Command(Option<AgentCommand>),
                            Detached(DetachedCompletion),
                            Base(Option<Result<BaseAgentEvent, AgentError>>),
                        }
                        let wake = future::or(
                            async { ActiveWake::Command(commands.recv().await.ok()) },
                            future::or(
                                async {
                                    ActiveWake::Detached(
                                        future::poll_fn(|context| {
                                            ephemeral.poll_completion(context)
                                        })
                                        .await,
                                    )
                                },
                                async { ActiveWake::Base(run.next().await) },
                            ),
                        )
                        .await;

                        match wake {
                            ActiveWake::Command(Some(AgentCommand::Dispatch(_))) => {
                                failure = Some(AgentError::StateInvariant);
                            }
                            ActiveWake::Command(Some(AgentCommand::Interrupt)) => run.interrupt(),
                            ActiveWake::Command(Some(AgentCommand::Cancel))
                            | ActiveWake::Command(None) => {
                                cancel_requested = true;
                                run.cancel();
                            }
                            ActiveWake::Command(Some(AgentCommand::ResolveApproval {
                                tool_call_id,
                                decision,
                            })) => {
                                let _ = run.resolve_approval(tool_call_id, decision);
                            }
                            ActiveWake::Detached(completion) => {
                                run.continue_with(Message::text(render_completions(
                                    std::slice::from_ref(&completion),
                                )));
                                pending_completions.push(completion);
                            }
                            ActiveWake::Base(Some(Ok(BaseAgentEvent::Iteration(progress)))) => {
                                if matches!(
                                    &progress,
                                    claw_utils::stream::StreamPart::Delta(
                                        AgentIterationEvent::Started(_)
                                    )
                                ) {
                                    applied_completions.append(&mut pending_completions);
                                }
                                yield AgentStreamItem::Event(Ok(AgentEvent::Iteration(progress)));
                            }
                            ActiveWake::Base(Some(Ok(BaseAgentEvent::Detached(handle)))) => {
                                ephemeral.push(handle);
                            }
                            ActiveWake::Base(Some(Ok(BaseAgentEvent::InputRequired(request)))) => {
                                let AgentInputRequest::Approval { tool_call_id, .. } = &request;
                                *awaiting_approval.borrow_mut() = Some(*tool_call_id);
                                yield AgentStreamItem::Event(Ok(AgentEvent::InputRequired(request)));
                            }
                            ActiveWake::Base(Some(Ok(BaseAgentEvent::Finished(finished)))) => {
                                outcome = Some(finished);
                            }
                            ActiveWake::Base(Some(Err(error))) => failure = Some(error),
                            ActiveWake::Base(None) => failure = Some(AgentError::StateInvariant),
                        }
                    }

                    drop(run);
                    *awaiting_approval.borrow_mut() = None;

                    if let Some(error) = failure {
                        ephemeral.clear();
                        yield AgentStreamItem::Event(Err(error));
                        break;
                    }

                    let outcome = outcome.expect("active BaseAgent loop exits with an outcome");
                    match &outcome {
                        AgentOutcome::Completed(_) => {
                            for completion in pending_completions.into_iter().rev() {
                                ephemeral.ready_detached_toolcalls.push_front(completion);
                            }
                        }
                        AgentOutcome::Interrupted => {
                            pending_completions.append(&mut applied_completions);
                            for completion in pending_completions.into_iter().rev() {
                                ephemeral.ready_detached_toolcalls.push_front(completion);
                            }
                        }
                        AgentOutcome::Cancelled => {
                            ephemeral.clear();
                            cancel_requested = true;
                        }
                    }
                    if cancel_requested {
                        activity.set(AgentActivity::Closed);
                    } else {
                        activity.set(AgentActivity::Idle);
                    }
                    yield AgentStreamItem::Event(Ok(AgentEvent::TurnEnded { outcome }));

                    if cancel_requested {
                        ephemeral.clear();
                        break;
                    }

                    let Some(next_turn) = ephemeral.next_turn(&commands, activity.as_ref()).await else {
                        break;
                    };
                    turn = next_turn;
                }
            }

            activity.set(AgentActivity::Closed);
            let agent = self
                .agent
                .take()
                .expect("a completed Agent stream returns its Agent");
            yield AgentStreamItem::Returned(agent);
        }
    }
}

impl<H: ClawHttp, Timer: ClawTimer> Drop for OwnedAgentGuard<H, Timer> {
    fn drop(&mut self) {
        self.activity.set(AgentActivity::Closed);
        if let Some(agent) = &mut self.agent {
            agent.ephemeral.clear();
        }
    }
}

fn detached_turn(completions: &mut VecDeque<DetachedCompletion>) -> Option<PendingTurn> {
    let first = completions.front()?.call.clone();
    let completions = completions.drain(..).collect::<Vec<_>>();
    Some(PendingTurn {
        origin: AgentTurnOrigin::ToolCall { call: first },
        message: Message::text(render_completions(&completions)),
        applied_completions: completions,
    })
}

fn render_completions(completions: &[DetachedCompletion]) -> String {
    let mut message = String::from("[detached:results]");
    for completion in completions {
        let (status, label) = if completion.output.ok {
            ("completed", "result")
        } else {
            ("failed", "error")
        };
        let _ = write!(
            message,
            "\n\n[detached:{status}]\ntool: {}\ncall_id: {}\n{label}:\n{}",
            completion.call.name, completion.call.id, completion.output.content,
        );
    }
    message
}

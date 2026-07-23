use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::VecDeque;
use std::fmt::Write as _;

use async_channel::{Receiver, TryRecvError};
use claw_api::ToolCall;
use claw_context::Context as AgentContext;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawHttp, ClawTimer};
use claw_persistence::DurableState;
use claw_tool::{ToolDetachCompletion, ToolDetachHandle, ToolExecution};
use futures_core::Stream;
use futures_lite::{future, StreamExt as _};

use super::base_agent::{
    AgentInputRequest, AgentOutcome, AgentSubmitError, BaseAgent, BaseAgentEvent,
};
use super::state::AgentState;
use super::stream::{AgentCommand, AgentEvent, AgentStream, AgentTurnOrigin};
use super::{AgentError, AgentIterationEvent};
use crate::session::Message;

#[derive(Clone)]
struct DetachedCompletion {
    call: ToolCall,
    execution: ToolExecution,
}

impl DetachedCompletion {
    fn from_tool(completion: ToolDetachCompletion) -> Self {
        let (invocation, execution) = completion.into_parts();
        Self {
            call: ToolCall {
                id: invocation.id().unwrap_or_default().to_owned(),
                name: invocation.name().to_owned(),
                arguments_json: invocation.arguments_json().to_owned(),
            },
            execution,
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
}

struct PendingTurn {
    origin: AgentTurnOrigin,
    message: Message,
    applied_completions: Vec<DetachedCompletion>,
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

    pub(crate) fn submit(&mut self, message: Message) -> AgentStream<'_> {
        let (control, commands, awaiting_approval) = AgentStream::channel();
        let stream = ActiveStreamGuard::new(self).into_stream(message, commands, awaiting_approval);
        AgentStream::new(stream, control)
    }

    pub(in crate::agent) fn state(&self) -> &DurableState<AgentState> {
        self.base.state()
    }

    pub(crate) fn context(&self) -> &AgentContext {
        self.base.context()
    }
}

struct ActiveStreamGuard<'a, H: ClawHttp, Timer: ClawTimer> {
    agent: &'a mut Agent<H, Timer>,
}

impl<'a, H, Timer> ActiveStreamGuard<'a, H, Timer>
where
    H: ClawHttp + StreamingHttp,
    Timer: ClawTimer,
{
    fn new(agent: &'a mut Agent<H, Timer>) -> Self {
        Self { agent }
    }

    fn into_stream(
        self,
        first_message: Message,
        commands: Receiver<AgentCommand>,
        awaiting_approval: std::rc::Rc<std::cell::RefCell<Option<super::ToolCallId>>>,
    ) -> impl futures_core::Stream<Item = Result<AgentEvent, AgentError>> + 'a {
        async_stream::stream! {
            let Agent { base, ephemeral } = self.agent;
            let mut messages = VecDeque::from([PendingTurn {
                origin: AgentTurnOrigin::Message,
                message: first_message,
                applied_completions: Vec::new(),
            }]);
            let mut cancel_requested = false;

            loop {
                let next_turn = loop {
                    loop {
                        match commands.try_recv() {
                            Ok(AgentCommand::Submit(message)) => {
                                messages.push_back(PendingTurn {
                                    origin: AgentTurnOrigin::Message,
                                    message,
                                    applied_completions: Vec::new(),
                                });
                            }
                            Ok(AgentCommand::Interrupt) => {}
                            Ok(AgentCommand::Cancel) => {
                                ephemeral.clear();
                                cancel_requested = true;
                            }
                            Ok(AgentCommand::ResolveApproval { .. }) => {}
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Closed) => {
                                ephemeral.clear();
                                cancel_requested = true;
                                break;
                            }
                        }
                    }
                    if cancel_requested {
                        break None;
                    }
                    if let Some(turn) = messages.pop_front() {
                        break Some(turn);
                    }
                    if !ephemeral.ready_detached_toolcalls.is_empty() {
                        break detached_turn(&mut ephemeral.ready_detached_toolcalls);
                    }
                    if !ephemeral.has_inflight_detached_toolcalls() {
                        break None;
                    }

                    enum IdleWake {
                        Command(Option<AgentCommand>),
                        Detached(DetachedCompletion),
                    }
                    let wake = future::or(
                        async { IdleWake::Command(commands.recv().await.ok()) },
                        async {
                            IdleWake::Detached(
                                future::poll_fn(|context| ephemeral.poll_completion(context)).await,
                            )
                        },
                    )
                    .await;
                    match wake {
                        IdleWake::Command(Some(AgentCommand::Submit(message))) => {
                            messages.push_back(PendingTurn {
                                origin: AgentTurnOrigin::Message,
                                message,
                                applied_completions: Vec::new(),
                            });
                        }
                        IdleWake::Command(Some(AgentCommand::Interrupt)) => {}
                        IdleWake::Command(Some(AgentCommand::Cancel))
                        | IdleWake::Command(None) => {
                            ephemeral.clear();
                            cancel_requested = true;
                            break None;
                        }
                        IdleWake::Command(Some(AgentCommand::ResolveApproval { .. })) => {}
                        IdleWake::Detached(completion) => {
                            ephemeral.ready_detached_toolcalls.push_back(completion);
                        }
                    }
                };

                let Some(turn) = next_turn else {
                    break;
                };
                yield Ok(AgentEvent::TurnStarted {
                    origin: turn.origin.clone(),
                });

                let mut run = match base.submit(turn.message) {
                    Ok(run) => run,
                    Err(AgentSubmitError::Transcript(error)) => {
                        yield Err(AgentError::Transcript(error));
                        ephemeral.clear();
                        break;
                    }
                    Err(AgentSubmitError::Running) => {
                        yield Err(AgentError::StateInvariant);
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
                        ActiveWake::Command(Some(AgentCommand::Submit(message))) => {
                            messages.push_back(PendingTurn {
                                origin: AgentTurnOrigin::Message,
                                message,
                                applied_completions: Vec::new(),
                            });
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
                            yield Ok(AgentEvent::Iteration(progress));
                        }
                        ActiveWake::Base(Some(Ok(BaseAgentEvent::Detached(handle)))) => {
                            ephemeral.push(handle);
                        }
                        ActiveWake::Base(Some(Ok(BaseAgentEvent::InputRequired(request)))) => {
                            let AgentInputRequest::Approval { tool_call_id, .. } = &request;
                            *awaiting_approval.borrow_mut() = Some(*tool_call_id);
                            yield Ok(AgentEvent::InputRequired(request));
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
                    yield Err(error);
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
                yield Ok(AgentEvent::TurnEnded { outcome });

                if cancel_requested {
                    ephemeral.clear();
                    break;
                }
            }
        }
    }
}

impl<H: ClawHttp, Timer: ClawTimer> Drop for ActiveStreamGuard<'_, H, Timer> {
    fn drop(&mut self) {
        self.agent.ephemeral.clear();
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
        let (status, label) = if completion.execution.ok {
            ("completed", "result")
        } else {
            ("failed", "error")
        };
        let _ = write!(
            message,
            "\n\n[detached:{status}]\ntool: {}\ncall_id: {}\n{label}:\n{}",
            completion.call.name, completion.call.id, completion.execution.content,
        );
    }
    message
}

use alloc::{borrow::ToOwned, collections::VecDeque, rc::Rc, string::String, vec::Vec};
use core::pin::Pin;
use core::task::{Context, Poll};
use core::{
    cell::{Cell, RefCell},
    fmt::Write as _,
};

use async_channel::{Receiver, TryRecvError};
use barracuda_agent_persistence::DurableState;
use barracuda_agent_tool::{ToolDetachHandle, ToolInvocation, ToolOutput};
use barracuda_model_api::ToolCall;
use futures_core::Stream;
use futures_lite::{future, StreamExt as _};
use futures_util::stream::SelectAll;
use http_client::embedded_nal_async::{Dns, TcpConnect};

use crate::agent_stream::{
    AgentActivity, AgentCommand, AgentEvent, AgentHandle, AgentStream, AgentStreamItem,
    AgentTurnOrigin,
};
use crate::engine::{
    AgentEngine, AgentEngineEvent, AgentInputRequest, AgentOutcome, AgentSubmitError,
};
use crate::engine::{AgentError, AgentIterationEvent};
use crate::AgentEngineState;
use crate::Message;
use barracuda_runtime_utils::yield_stream::yield_stream;

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
    inflight_detached_toolcalls: SelectAll<ToolDetachHandle>,
    ready_detached_toolcalls: VecDeque<DetachedCompletion>,
}

impl AgentEphemeralState {
    fn new() -> Self {
        Self {
            inflight_detached_toolcalls: SelectAll::new(),
            ready_detached_toolcalls: VecDeque::new(),
        }
    }

    fn push(&mut self, handle: ToolDetachHandle) {
        self.inflight_detached_toolcalls.push(handle);
    }

    fn poll_completion(&mut self, context: &mut Context<'_>) -> Poll<DetachedCompletion> {
        match Pin::new(&mut self.inflight_detached_toolcalls).poll_next(context) {
            Poll::Ready(Some(completion)) => Poll::Ready(DetachedCompletion::from_tool(completion)),
            // An exhausted set yields `None`; no completion can arrive until
            // another detached handle is pushed, so keep the wait pending.
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
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

/// One long-lived Agent instance around the single-task `AgentEngine` core.
pub struct Agent<Tcp = http_client::Tcp, Resolver = http_client::Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    engine: AgentEngine<Tcp, Resolver>,
    ephemeral: AgentEphemeralState,
}

impl<Tcp, Resolver> Agent<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    pub(super) fn new(engine: AgentEngine<Tcp, Resolver>) -> Self {
        Self {
            engine,
            ephemeral: AgentEphemeralState::new(),
        }
    }

    pub fn into_stream(self, message: Message) -> (AgentStream<Tcp, Resolver>, AgentHandle) {
        let (handle, commands, activity, awaiting_approval) = AgentHandle::channel();
        let stream =
            OwnedAgentGuard::new(self, activity).into_stream(message, commands, awaiting_approval);
        (AgentStream::new(stream), handle)
    }

    pub(crate) fn state(&self) -> &DurableState<AgentEngineState> {
        self.engine.state()
    }
}

struct OwnedAgentGuard<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    agent: Option<Agent<Tcp, Resolver>>,
    activity: Rc<Cell<AgentActivity>>,
}

impl<Tcp, Resolver> OwnedAgentGuard<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    fn new(agent: Agent<Tcp, Resolver>, activity: Rc<Cell<AgentActivity>>) -> Self {
        Self {
            agent: Some(agent),
            activity,
        }
    }

    fn into_stream(
        mut self,
        first_message: Message,
        commands: Receiver<AgentCommand>,
        awaiting_approval: Rc<RefCell<Option<super::ToolCallId>>>,
    ) -> impl futures_core::Stream<Item = AgentStreamItem<Tcp, Resolver>> + 'static {
        yield_stream(|yielder| async move {
            let activity = Rc::clone(&self.activity);
            {
                let Some(Agent { engine, ephemeral }) = self.agent.as_mut() else {
                    activity.set(AgentActivity::Closed);
                    yielder
                        .yield_one(AgentStreamItem::Event(Err(AgentError::StateInvariant)))
                        .await;
                    return;
                };
                let mut turn = PendingTurn::message(first_message);
                let mut cancel_requested = false;

                loop {
                    yielder
                        .yield_one(AgentStreamItem::Event(Ok(AgentEvent::TurnStarted {
                            origin: turn.origin.clone(),
                        })))
                        .await;

                    let mut run = match engine.submit(turn.message) {
                        Ok(run) => run,
                        Err(AgentSubmitError::Transcript(error)) => {
                            yielder
                                .yield_one(AgentStreamItem::Event(Err(AgentError::Transcript(
                                    error,
                                ))))
                                .await;
                            ephemeral.clear();
                            break;
                        }
                        Err(AgentSubmitError::Running) => {
                            yielder
                                .yield_one(AgentStreamItem::Event(Err(AgentError::StateInvariant)))
                                .await;
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
                            Engine(Option<Result<AgentEngineEvent, AgentError>>),
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
                                async { ActiveWake::Engine(run.next().await) },
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
                                    core::slice::from_ref(&completion),
                                )));
                                pending_completions.push(completion);
                            }
                            ActiveWake::Engine(Some(Ok(AgentEngineEvent::Iteration(progress)))) => {
                                if matches!(
                                    &progress,
                                    barracuda_runtime_utils::stream::StreamPart::Delta(
                                        AgentIterationEvent::Started(_)
                                    )
                                ) {
                                    applied_completions.append(&mut pending_completions);
                                }
                                yielder
                                    .yield_one(AgentStreamItem::Event(Ok(AgentEvent::Iteration(
                                        progress,
                                    ))))
                                    .await;
                            }
                            ActiveWake::Engine(Some(Ok(AgentEngineEvent::Detached(handle)))) => {
                                ephemeral.push(handle);
                            }
                            ActiveWake::Engine(Some(Ok(AgentEngineEvent::InputRequired(
                                request,
                            )))) => {
                                let AgentInputRequest::Approval { tool_call_id, .. } = &request;
                                *awaiting_approval.borrow_mut() = Some(*tool_call_id);
                                yielder
                                    .yield_one(AgentStreamItem::Event(Ok(
                                        AgentEvent::InputRequired(request),
                                    )))
                                    .await;
                            }
                            ActiveWake::Engine(Some(Ok(AgentEngineEvent::Finished(finished)))) => {
                                outcome = Some(finished);
                            }
                            ActiveWake::Engine(Some(Err(error))) => failure = Some(error),
                            ActiveWake::Engine(None) => failure = Some(AgentError::StateInvariant),
                        }
                    }

                    drop(run);
                    *awaiting_approval.borrow_mut() = None;

                    if let Some(error) = failure {
                        ephemeral.clear();
                        yielder.yield_one(AgentStreamItem::Event(Err(error))).await;
                        break;
                    }

                    let Some(outcome) = outcome else {
                        ephemeral.clear();
                        yielder
                            .yield_one(AgentStreamItem::Event(Err(AgentError::StateInvariant)))
                            .await;
                        break;
                    };
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
                    yielder
                        .yield_one(AgentStreamItem::Event(Ok(AgentEvent::TurnEnded {
                            outcome,
                        })))
                        .await;

                    if cancel_requested {
                        ephemeral.clear();
                        break;
                    }

                    let Some(next_turn) = ephemeral.next_turn(&commands, activity.as_ref()).await
                    else {
                        break;
                    };
                    turn = next_turn;
                }
            }

            activity.set(AgentActivity::Closed);
            if let Some(agent) = self.agent.take() {
                yielder.yield_one(AgentStreamItem::Returned(agent)).await;
            } else {
                yielder
                    .yield_one(AgentStreamItem::Event(Err(AgentError::StateInvariant)))
                    .await;
            }
        })
    }
}

impl<Tcp, Resolver> Drop for OwnedAgentGuard<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
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

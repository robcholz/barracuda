use alloc::{borrow::ToOwned, collections::VecDeque, rc::Rc, string::String, vec::Vec};
use core::pin::Pin;
use core::task::{Context, Poll};
use core::{
    cell::{Cell, RefCell},
    fmt::Write as _,
};

use barracuda_agent_persistence::DurableState;
use barracuda_agent_tool::{ToolDetachHandle, ToolDetachUpdate, ToolInvocation, ToolOutput};
use barracuda_model_api::ToolCall;
use barracuda_runtime_utils::local_channel::{Receiver, TryRecvError};
use barracuda_runtime_utils::unordered::Merged;
use futures_core::Stream;
use futures_lite::{future, StreamExt as _};
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
    fn into_notification(self) -> DetachedNotification {
        DetachedNotification {
            call: self.call,
            update: ToolDetachUpdate::Completed(self.output),
        }
    }
}

#[derive(Clone)]
struct DetachedNotification {
    call: ToolCall,
    update: ToolDetachUpdate,
}

impl DetachedNotification {
    fn from_tool((invocation, update): (ToolInvocation, ToolDetachUpdate)) -> Self {
        Self {
            call: ToolCall {
                id: invocation.id().unwrap_or_default().to_owned(),
                name: invocation.name().to_owned(),
                arguments_json: invocation.arguments_json().to_owned(),
            },
            update,
        }
    }

    fn completion(&self) -> Option<DetachedCompletion> {
        let ToolDetachUpdate::Completed(output) = &self.update else {
            return None;
        };
        Some(DetachedCompletion {
            call: self.call.clone(),
            output: output.clone(),
        })
    }
}

/// Runtime-only state that disappears when this Agent is dropped or restarted.
struct AgentEphemeralState {
    inflight_detached_toolcalls: Merged<ToolDetachHandle>,
    ready_detached_toolcalls: VecDeque<DetachedNotification>,
}

impl AgentEphemeralState {
    fn new() -> Self {
        Self {
            inflight_detached_toolcalls: Merged::new(),
            ready_detached_toolcalls: VecDeque::new(),
        }
    }

    fn push(&mut self, handle: ToolDetachHandle) {
        self.inflight_detached_toolcalls.push(handle);
    }

    fn poll_update(&mut self, context: &mut Context<'_>) -> Poll<DetachedNotification> {
        match Pin::new(&mut self.inflight_detached_toolcalls).poll_next(context) {
            Poll::Ready(Some(update)) => Poll::Ready(DetachedNotification::from_tool(update)),
            // An exhausted set yields `None`; no update can arrive until
            // another detached handle is pushed, so keep waiting.
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
                Detached(DetachedNotification),
            }
            match future::or(async { Wake::Command(commands.recv().await) }, async {
                Wake::Detached(future::poll_fn(|context| self.poll_update(context)).await)
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
                Wake::Detached(update) => {
                    self.ready_detached_toolcalls.push_back(update);
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
                    let mut pending_updates = Vec::new();
                    let mut outcome = None;
                    let mut failure = None;

                    while outcome.is_none() && failure.is_none() {
                        enum ActiveWake {
                            Command(Option<AgentCommand>),
                            Detached(DetachedNotification),
                            Engine(Option<Result<AgentEngineEvent, AgentError>>),
                        }
                        let wake = future::or(
                            async { ActiveWake::Command(commands.recv().await) },
                            future::or(
                                async {
                                    ActiveWake::Detached(
                                        future::poll_fn(|context| ephemeral.poll_update(context))
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
                            ActiveWake::Detached(update) => {
                                run.continue_with(Message::text(render_notifications(
                                    core::slice::from_ref(&update),
                                )));
                                pending_updates.push(update);
                            }
                            ActiveWake::Engine(Some(Ok(AgentEngineEvent::Iteration(progress)))) => {
                                if matches!(
                                    &progress,
                                    barracuda_runtime_utils::stream::StreamPart::Delta(
                                        AgentIterationEvent::Started(_)
                                    )
                                ) {
                                    for update in pending_updates.drain(..) {
                                        if let Some(completion) = update.completion() {
                                            applied_completions.push(completion);
                                        }
                                    }
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
                            for update in pending_updates.into_iter().rev() {
                                ephemeral.ready_detached_toolcalls.push_front(update);
                            }
                        }
                        AgentOutcome::Interrupted => {
                            pending_updates.extend(
                                applied_completions
                                    .drain(..)
                                    .map(DetachedCompletion::into_notification),
                            );
                            for update in pending_updates.into_iter().rev() {
                                ephemeral.ready_detached_toolcalls.push_front(update);
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

fn detached_turn(notifications: &mut VecDeque<DetachedNotification>) -> Option<PendingTurn> {
    let first = notifications.front()?.call.clone();
    let notifications = notifications.drain(..).collect::<Vec<_>>();
    let applied_completions = notifications
        .iter()
        .filter_map(DetachedNotification::completion)
        .collect();
    Some(PendingTurn {
        origin: AgentTurnOrigin::ToolCall { call: first },
        message: Message::text(render_notifications(&notifications)),
        applied_completions,
    })
}

fn render_notifications(notifications: &[DetachedNotification]) -> String {
    let mut message = String::from("[detached:updates]");
    for notification in notifications {
        let (status, label, output) = match &notification.update {
            ToolDetachUpdate::Progress(output) => ("progress", "update", output),
            ToolDetachUpdate::Completed(output) if output.ok => ("completed", "result", output),
            ToolDetachUpdate::Completed(output) => ("failed", "error", output),
        };
        let _ = write!(
            message,
            "\n\n[detached:{status}]\ntool: {}\ncall_id: {}\n{label}:\n{}",
            notification.call.name, notification.call.id, output.content,
        );
    }
    message
}

#[cfg(test)]
mod tests {
    use alloc::{collections::VecDeque, string::ToString, vec};

    use barracuda_agent_tool::{ToolDetachUpdate, ToolOutput};
    use barracuda_model_api::ToolCall;

    use super::{detached_turn, render_notifications, DetachedNotification};

    fn notification(update: ToolDetachUpdate) -> DetachedNotification {
        DetachedNotification {
            call: ToolCall {
                id: "call-1".to_string(),
                name: "background".to_string(),
                arguments_json: "{}".to_string(),
            },
            update,
        }
    }

    #[test]
    fn progress_is_rendered_as_non_terminal_update() {
        let rendered =
            render_notifications(&[notification(ToolDetachUpdate::Progress(ToolOutput {
                content: "input_required".to_string(),
                ok: true,
            }))]);

        assert!(rendered.contains("[detached:progress]"));
        assert!(rendered.contains("update:\ninput_required"));
        assert!(!rendered.contains("[detached:completed]"));
    }

    #[test]
    fn detached_turn_tracks_only_terminal_updates_as_applied_completions() {
        let mut notifications = VecDeque::from(vec![
            notification(ToolDetachUpdate::Progress(ToolOutput {
                content: "working".to_string(),
                ok: true,
            })),
            notification(ToolDetachUpdate::Completed(ToolOutput {
                content: "done".to_string(),
                ok: true,
            })),
        ]);

        let Some(turn) = detached_turn(&mut notifications) else {
            return;
        };
        assert_eq!(turn.applied_completions.len(), 1);
        assert!(turn.message.as_str().contains("[detached:progress]"));
        assert!(turn.message.as_str().contains("[detached:completed]"));
    }
}

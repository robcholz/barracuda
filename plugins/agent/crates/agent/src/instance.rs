use alloc::{borrow::ToOwned, collections::VecDeque, rc::Rc, string::String, vec::Vec};
use core::task::{Context, Poll};
use core::{
    cell::{Cell, RefCell},
    fmt::Write as _,
};

use barracuda_agent_persistence::DurableState;
use barracuda_agent_tool::{BackgroundToolEvent, BackgroundToolPool, BackgroundToolUpdate};
use barracuda_model_api::ToolCall;
use barracuda_runtime_utils::local_channel::{Receiver, TryRecvError};
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

/// One background pool update waiting to reach the model.
#[derive(Clone)]
struct BackgroundNotification {
    id: u32,
    call: ToolCall,
    update: BackgroundToolUpdate,
}

impl BackgroundNotification {
    fn is_completion(&self) -> bool {
        matches!(self.update, BackgroundToolUpdate::Completed(_))
    }
}

impl From<BackgroundToolEvent> for BackgroundNotification {
    fn from(event: BackgroundToolEvent) -> Self {
        Self {
            id: event.id,
            call: ToolCall {
                id: event.invocation.id().unwrap_or_default().to_owned(),
                name: event.invocation.name().to_owned(),
                arguments_json: event.invocation.arguments_json().to_owned(),
            },
            update: event.update,
        }
    }
}

/// Runtime-only state that disappears when this Agent is dropped or restarted.
struct AgentEphemeralState {
    background: BackgroundToolPool,
    ready_background_updates: VecDeque<BackgroundNotification>,
}

impl AgentEphemeralState {
    fn new(background: BackgroundToolPool) -> Self {
        Self {
            background,
            ready_background_updates: VecDeque::new(),
        }
    }

    fn poll_update(&mut self, context: &mut Context<'_>) -> Poll<BackgroundNotification> {
        self.background.poll_next(context).map(Into::into)
    }

    fn clear(&mut self) {
        self.background.clear();
        self.ready_background_updates.clear();
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

            if !self.ready_background_updates.is_empty() && activity.get() == AgentActivity::Idle {
                activity.set(AgentActivity::Running);
                return background_turn(&mut self.ready_background_updates);
            }
            if self.background.is_empty() {
                return None;
            }

            enum Wake {
                Command(Option<AgentCommand>),
                Background(BackgroundNotification),
            }
            match future::or(async { Wake::Command(commands.recv().await) }, async {
                Wake::Background(future::poll_fn(|context| self.poll_update(context)).await)
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
                Wake::Background(update) => {
                    self.ready_background_updates.push_back(update);
                }
            }
        }
    }
}

struct PendingTurn {
    origin: AgentTurnOrigin,
    message: Message,
    /// Background completions this turn delivers; re-queued if it is
    /// interrupted.
    applied_completions: Vec<BackgroundNotification>,
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
    pub(super) fn new(engine: AgentEngine<Tcp, Resolver>, background: BackgroundToolPool) -> Self {
        Self {
            engine,
            ephemeral: AgentEphemeralState::new(background),
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
                            Background(BackgroundNotification),
                            Engine(Option<Result<AgentEngineEvent, AgentError>>),
                        }
                        let wake = future::or(
                            async { ActiveWake::Command(commands.recv().await) },
                            future::or(
                                async {
                                    ActiveWake::Background(
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
                            ActiveWake::Background(update) => {
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
                                    applied_completions.extend(
                                        pending_updates
                                            .drain(..)
                                            .filter(BackgroundNotification::is_completion),
                                    );
                                }
                                yielder
                                    .yield_one(AgentStreamItem::Event(Ok(AgentEvent::Iteration(
                                        progress,
                                    ))))
                                    .await;
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
                                ephemeral.ready_background_updates.push_front(update);
                            }
                        }
                        AgentOutcome::Interrupted => {
                            pending_updates.append(&mut applied_completions);
                            for update in pending_updates.into_iter().rev() {
                                ephemeral.ready_background_updates.push_front(update);
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

fn background_turn(notifications: &mut VecDeque<BackgroundNotification>) -> Option<PendingTurn> {
    let first = notifications.front()?.call.clone();
    let notifications = notifications.drain(..).collect::<Vec<_>>();
    let message = Message::text(render_notifications(&notifications));
    let applied_completions = notifications
        .into_iter()
        .filter(BackgroundNotification::is_completion)
        .collect();
    Some(PendingTurn {
        origin: AgentTurnOrigin::ToolCall { call: first },
        message,
        applied_completions,
    })
}

fn render_notifications(notifications: &[BackgroundNotification]) -> String {
    let mut message = String::from("[background:updates]");
    for notification in notifications {
        let (status, label, output) = match &notification.update {
            BackgroundToolUpdate::Progress(output) => ("progress", "update", output),
            BackgroundToolUpdate::Completed(output) if output.ok => ("completed", "result", output),
            BackgroundToolUpdate::Completed(output) => ("failed", "error", output),
        };
        let _ = write!(
            message,
            "\n\n[background:{status}]\nid: {}\ntool: {}\n{label}:\n{}",
            notification.id, notification.call.name, output.content,
        );
    }
    message
}

#[cfg(test)]
mod tests {
    use alloc::{collections::VecDeque, string::ToString, vec};

    use barracuda_agent_tool::{BackgroundToolUpdate, ToolOutput};
    use barracuda_model_api::ToolCall;

    use super::{background_turn, render_notifications, BackgroundNotification};

    fn notification(update: BackgroundToolUpdate) -> BackgroundNotification {
        BackgroundNotification {
            id: 7,
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
            render_notifications(&[notification(BackgroundToolUpdate::Progress(ToolOutput {
                content: "input_required".to_string(),
                ok: true,
            }))]);

        assert!(rendered.contains("[background:progress]\nid: 7\ntool: background"));
        assert!(rendered.contains("update:\ninput_required"));
        assert!(!rendered.contains("[background:completed]"));
    }

    #[test]
    fn background_turn_tracks_only_terminal_updates_as_applied_completions() {
        let mut notifications = VecDeque::from(vec![
            notification(BackgroundToolUpdate::Progress(ToolOutput {
                content: "working".to_string(),
                ok: true,
            })),
            notification(BackgroundToolUpdate::Completed(ToolOutput {
                content: "done".to_string(),
                ok: true,
            })),
        ]);

        let Some(turn) = background_turn(&mut notifications) else {
            return;
        };
        assert_eq!(turn.applied_completions.len(), 1);
        assert!(turn.message.as_str().contains("[background:progress]"));
        assert!(turn.message.as_str().contains("[background:completed]"));
    }
}

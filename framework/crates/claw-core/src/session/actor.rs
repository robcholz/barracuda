use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use async_channel::{Receiver, Sender};
use claw_api::ToolCall;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_persistence::DurableState;
use claw_utils::stream::StreamPart;
use futures_core::Stream;

use super::agent_slot::{AgentSlot, AgentSlotUpdate, AgentSlots};
use super::approval::{
    ApprovalCompletion, ApprovalFlow, ApprovalRespondError, LlmApprovalResolver,
    SharedApprovalResolver,
};
use super::control::{ControlOp, SessionCommand, SessionControlError};
use super::manager::{OpenSessionError, SessionDeleteError, SharedAgentManager};
use super::permission::SessionPermission;
use super::state::{AgentIdAllocatorHandle, SessionPersistentState};
use super::{
    InputRequestId, IterationEvent, Message, SessionCloseReason, SessionEvent, SessionEventError,
    SessionId, SessionInputError, SessionPersistence, SessionTurnError, TurnEvent, TurnEventError,
    TurnId, TurnOrigin,
};
use crate::agent::{
    AgentCompletion, AgentCreateError, AgentEvent, AgentInputRequest, AgentIterationEvent,
    AgentOutcome, AgentTurnOrigin, ApprovalDecision, PersistenceConfig, ReasoningEffort,
    ToolCallId,
};
use crate::scheduler::{AgentRunPort, AgentRunSchedulerHandle};

struct OpenSession {
    lease: u64,
    events: Sender<SessionEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopReason {
    Close,
    Shutdown,
    Delete,
}

impl StopReason {
    fn escalate(self, incoming: Self) -> Self {
        match (self, incoming) {
            (Self::Delete, _) | (_, Self::Delete) => Self::Delete,
            (Self::Shutdown, _) | (_, Self::Shutdown) => Self::Shutdown,
            (Self::Close, Self::Close) => Self::Close,
        }
    }
}

struct Stopping {
    reason: StopReason,
    close_acks: Vec<Sender<Result<(), SessionControlError>>>,
    delete_ack: Option<Sender<Result<(), SessionDeleteError>>>,
}

enum ActorLifecycle {
    Running,
    Stopping(Stopping),
    DeleteReady(Stopping),
}

impl ActorLifecycle {
    fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }

    fn is_deleting(&self) -> bool {
        matches!(
            self,
            Self::Stopping(Stopping {
                reason: StopReason::Delete,
                ..
            }) | Self::DeleteReady(_)
        )
    }

    fn stop(
        &mut self,
        reason: StopReason,
        close_ack: Option<Sender<Result<(), SessionControlError>>>,
        delete_ack: Option<Sender<Result<(), SessionDeleteError>>>,
    ) {
        match self {
            Self::Running => {
                *self = Self::Stopping(Stopping {
                    reason,
                    close_acks: close_ack.into_iter().collect(),
                    delete_ack,
                });
            }
            Self::Stopping(stopping) => {
                stopping.reason = stopping.reason.escalate(reason);
                stopping.close_acks.extend(close_ack);
                debug_assert!(
                    stopping.delete_ack.is_none() || delete_ack.is_none(),
                    "one Session deletion has only one completion receiver"
                );
                if delete_ack.is_some() {
                    stopping.delete_ack = delete_ack;
                }
            }
            Self::DeleteReady(_) => {
                unreachable!("a delete-ready actor is completed synchronously by its manager")
            }
        }
    }

    fn reason(&self) -> Option<StopReason> {
        match self {
            Self::Running | Self::DeleteReady(_) => None,
            Self::Stopping(stopping) => Some(stopping.reason),
        }
    }
}

#[derive(Clone, Copy)]
enum PollSource {
    Command,
    Approval,
    Agent,
}

impl PollSource {
    fn next(self) -> Self {
        match self {
            Self::Command => Self::Approval,
            Self::Approval => Self::Agent,
            Self::Agent => Self::Command,
        }
    }
}

pub(super) enum SessionActorExit {
    DeleteReady { session: SessionId },
    Shutdown { session: SessionId },
}

impl SessionActorExit {
    pub(super) fn session(&self) -> SessionId {
        match self {
            Self::DeleteReady { session } | Self::Shutdown { session } => *session,
        }
    }
}

/// One long-lived Session stream backed by Session-owned Agent slots.
///
/// The actor never polls an Agent directly. It moves each resident Agent into
/// the process-global Scheduler, reduces scheduler outputs back into its slot,
/// and projects only root-Agent events onto the public Session stream.
pub(super) struct SessionActor<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    session: SessionId,
    persistence: SessionPersistence,
    state: DurableState<SessionPersistentState>,
    agent_manager: SharedAgentManager<Filesystem, Http, Timer>,
    agent_id_allocator: AgentIdAllocatorHandle,

    agents: AgentSlots<Http, Timer>,
    inbox: VecDeque<Message>,
    active_turn: Option<TurnId>,
    next_turn: u32,
    approval: ApprovalFlow<LlmApprovalResolver<Http, Timer>>,

    run_port: AgentRunPort<Http, Timer>,
    commands: Pin<Box<Receiver<SessionCommand>>>,
    next_source: PollSource,

    client: Option<OpenSession>,
    next_lease: u64,
    lifecycle: ActorLifecycle,
}

impl<Filesystem, Http, Timer> SessionActor<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        session: SessionId,
        persistence: SessionPersistence,
        agent_manager: SharedAgentManager<Filesystem, Http, Timer>,
        agent_id_allocator: AgentIdAllocatorHandle,
        state: DurableState<SessionPersistentState>,
        approval_resolver: SharedApprovalResolver<Http, Timer>,
        scheduler: AgentRunSchedulerHandle<Http, Timer>,
    ) -> (Self, Sender<SessionCommand>) {
        let (command_sender, commands) = async_channel::unbounded();
        (
            Self {
                session,
                persistence,
                state,
                agent_manager,
                agent_id_allocator,
                agents: AgentSlots::new(),
                inbox: VecDeque::new(),
                active_turn: None,
                next_turn: 1,
                approval: ApprovalFlow::new(approval_resolver),
                run_port: AgentRunPort::new(scheduler),
                commands: Box::pin(commands),
                next_source: PollSource::Command,
                client: None,
                next_lease: 1,
                lifecycle: ActorLifecycle::Running,
            },
            command_sender,
        )
    }

    pub(super) fn poll(&mut self, context: &mut Context<'_>) -> Poll<SessionActorStatus> {
        if let Some(exit) = self.finish_lifecycle() {
            return Poll::Ready(SessionActorStatus::Exit(exit));
        }

        for _ in 0..3 {
            let source = self.next_source;
            self.next_source = source.next();
            match source {
                PollSource::Command => {
                    if let Poll::Ready(command) = self.commands.as_mut().poll_next(context) {
                        match command {
                            Some(command) => self.handle_command(command),
                            None => self.request_shutdown(),
                        }
                        return Poll::Ready(SessionActorStatus::Progress);
                    }
                }
                PollSource::Approval => {
                    if let Poll::Ready(Some(completion)) = self.approval.poll(context) {
                        self.handle_approval_result(completion);
                        return Poll::Ready(SessionActorStatus::Progress);
                    }
                }
                PollSource::Agent => {
                    if let Poll::Ready(Some(output)) =
                        Pin::new(&mut self.run_port).poll_next(context)
                    {
                        self.handle_agent_output(output);
                        return Poll::Ready(SessionActorStatus::Progress);
                    }
                }
            }
        }

        if self.start_next_message() {
            return Poll::Ready(SessionActorStatus::Progress);
        }

        Poll::Pending
    }

    fn handle_command(&mut self, command: SessionCommand) {
        match command {
            SessionCommand::Append {
                lease,
                message,
                ack,
            } => self.append(lease, message, ack),
            SessionCommand::Respond {
                lease,
                request,
                message,
                ack,
            } => self.respond(lease, request, message, ack),
            SessionCommand::Control { lease, op, ack } => self.control(lease, op, ack),
            SessionCommand::SetReasoningEffort { lease, effort, ack } => {
                if self.accepts(lease) {
                    self.set_reasoning_effort(effort);
                    let _ = ack.try_send(Ok(()));
                } else {
                    self.reject_closed(ack);
                }
            }
            SessionCommand::SetPermissionLevel { lease, level, ack } => {
                if self.accepts(lease) {
                    if self.state.get().permission_level != level {
                        self.state.get_mut().permission_level = level;
                    }
                    let _ = ack.try_send(Ok(()));
                } else {
                    self.reject_closed(ack);
                }
            }
            SessionCommand::Close { lease, ack } => self.close(lease, ack),
        }
    }

    pub(super) fn open(&mut self, events: Sender<SessionEvent>) -> Result<u64, OpenSessionError> {
        if self.client.is_some() || !self.lifecycle.is_running() {
            return Err(OpenSessionError::AlreadyOpen(self.session));
        }
        let lease = self.next_lease;
        self.next_lease = self.next_lease.saturating_add(1);
        self.client = Some(OpenSession { lease, events });
        Ok(lease)
    }

    fn append(
        &mut self,
        lease: u64,
        message: Message,
        ack: Sender<Result<(), SessionControlError>>,
    ) {
        if !self.accepts(lease) {
            self.reject_closed(ack);
            return;
        }
        self.inbox.push_back(message);
        let _ = ack.try_send(Ok(()));
    }

    fn respond(
        &mut self,
        lease: u64,
        request: InputRequestId,
        message: Message,
        ack: Sender<Result<(), SessionControlError>>,
    ) {
        if !self.accepts(lease) {
            self.reject_closed(ack);
            return;
        }
        let result = self
            .approval
            .respond(request, message)
            .map_err(|error| match error {
                ApprovalRespondError::NotWaiting | ApprovalRespondError::Resolving => {
                    SessionControlError::NotAwaitingInput(self.session)
                }
                ApprovalRespondError::RequestMismatch { expected } => {
                    SessionControlError::InputRequestMismatch {
                        session: self.session,
                        expected,
                        received: request,
                    }
                }
            });
        let _ = ack.try_send(result);
    }

    fn control(&mut self, lease: u64, op: ControlOp, ack: Sender<Result<(), SessionControlError>>) {
        if !self.accepts(lease) {
            self.reject_closed(ack);
            return;
        }
        match op {
            ControlOp::Interrupt => {
                if let Some(root) = self.root_mut() {
                    root.interrupt();
                }
            }
            ControlOp::Cancel => {
                if let Some(root) = self.root_mut() {
                    root.cancel();
                }
            }
        }
        self.approval.cancel();
        let _ = ack.try_send(Ok(()));
    }

    fn close(&mut self, lease: u64, ack: Sender<Result<(), SessionControlError>>) {
        if !self.accepts(lease) {
            self.reject_closed(ack);
            return;
        }
        self.lifecycle.stop(StopReason::Close, Some(ack), None);
        self.stop_current_run(false);
    }

    pub(super) fn request_delete(&mut self, ack: Sender<Result<(), SessionDeleteError>>) {
        if self.lifecycle.is_deleting() {
            let _ = ack.try_send(Err(SessionDeleteError::AlreadyDeleting(self.session)));
            return;
        }
        self.lifecycle.stop(StopReason::Delete, None, Some(ack));
        self.stop_current_run(true);
    }

    pub(super) fn request_shutdown(&mut self) {
        self.lifecycle.stop(StopReason::Shutdown, None, None);
        self.stop_current_run(false);
    }

    fn stop_current_run(&mut self, reaping: bool) {
        self.inbox.clear();
        self.approval.cancel();
        for agent in self.agents.values_mut() {
            if reaping {
                agent.begin_reaping();
            } else {
                agent.cancel();
            }
        }
    }

    fn finish_lifecycle(&mut self) -> Option<SessionActorExit> {
        let reason = self.lifecycle.reason()?;
        if self.agents.values().any(AgentSlot::is_in_flight) {
            return None;
        }
        self.finish_turn();

        let ActorLifecycle::Stopping(stopping) =
            std::mem::replace(&mut self.lifecycle, ActorLifecycle::Running)
        else {
            unreachable!("a stop reason comes only from a stopping lifecycle")
        };

        match reason {
            StopReason::Delete => {
                if let Err(error) = self.delete_agents() {
                    self.fail_delete(stopping, error.into());
                    return None;
                }
                self.lifecycle = ActorLifecycle::DeleteReady(stopping);
                Some(SessionActorExit::DeleteReady {
                    session: self.session,
                })
            }
            StopReason::Shutdown => {
                self.emit_closed(SessionCloseReason::RuntimeShutdown);
                Self::complete_close_requests(stopping.close_acks);
                Some(SessionActorExit::Shutdown {
                    session: self.session,
                })
            }
            StopReason::Close => {
                self.emit_closed(SessionCloseReason::Requested);
                Self::complete_close_requests(stopping.close_acks);
                None
            }
        }
    }

    pub(super) fn complete_delete(&mut self, result: Result<(), SessionDeleteError>) -> bool {
        let ActorLifecycle::DeleteReady(mut stopping) =
            std::mem::replace(&mut self.lifecycle, ActorLifecycle::Running)
        else {
            unreachable!("only a delete-ready actor can complete Session deletion")
        };
        match result {
            Ok(()) => {
                self.emit_closed(SessionCloseReason::Deleted);
                Self::complete_close_requests(stopping.close_acks);
                if let Some(ack) = stopping.delete_ack.take() {
                    let _ = ack.try_send(Ok(()));
                }
                true
            }
            Err(error) => {
                self.fail_delete(stopping, error);
                false
            }
        }
    }

    fn fail_delete(&mut self, mut stopping: Stopping, error: SessionDeleteError) {
        self.emit_event_error(SessionEventError::DeleteFailed);
        if let Some(ack) = stopping.delete_ack.take() {
            let _ = ack.try_send(Err(error));
        }
        if !stopping.close_acks.is_empty() {
            self.lifecycle = ActorLifecycle::Stopping(Stopping {
                reason: StopReason::Close,
                close_acks: stopping.close_acks,
                delete_ack: None,
            });
        }
    }

    fn complete_close_requests(acks: Vec<Sender<Result<(), SessionControlError>>>) {
        for ack in acks {
            let _ = ack.try_send(Ok(()));
        }
    }

    fn start_next_message(&mut self) -> bool {
        if !self.lifecycle.is_running() {
            return false;
        }
        if self.active_turn.is_some() || self.inbox.is_empty() {
            return false;
        }
        let Some(message) = self.inbox.pop_front() else {
            return false;
        };
        if message.as_str().trim().is_empty() {
            return true;
        }
        if let Err(error) = self.ensure_root() {
            self.begin_turn(TurnOrigin::User);
            self.emit_turn_error(error.into());
            self.finish_turn();
            return true;
        }
        match self.dispatch_root(message) {
            Ok(()) => true,
            Err(message) => {
                self.inbox.push_front(message);
                false
            }
        }
    }

    fn dispatch_root(&mut self, message: Message) -> Result<(), Message> {
        let agent = self
            .root_id()
            .expect("an Agent run starts only with a root Agent id");
        let span = tracing::info_span!(
            "agent",
            trace.task = %agent,
            run.agent = %agent,
        );
        let root = self
            .agents
            .get_mut(&agent)
            .expect("an Agent run starts only with a resident root");
        root.dispatch(message, &self.run_port, span)
    }

    fn delete_agents(&mut self) -> Result<(), AgentCreateError> {
        let root_agent = self.root_id();
        let mut agent_ids = self.agents.keys().copied().collect::<BTreeSet<_>>();
        agent_ids.extend(root_agent);
        // Drop every live component handle before deleting its canonical
        // stores. In particular, dropping a filesystem TranscriptStore after
        // deletion could otherwise recreate its index file.
        self.agents.clear();
        for agent in agent_ids {
            self.agent_manager.remove(agent)?;
            if Some(agent) == root_agent {
                self.state.get_mut().clear_root();
            }
        }
        Ok(())
    }

    fn ensure_root(&mut self) -> Result<(), AgentCreateError> {
        if self
            .root_id()
            .is_some_and(|root| self.agents.contains_key(&root))
        {
            return Ok(());
        }
        let reasoning_effort = self.state.get().reasoning_effort;
        let permission = Arc::new(SessionPermission::new(self.state.clone()));
        let root_agent = self.state.get().root_agent;
        let (id, agent, reasoning_handle) = if let Some(id) = root_agent {
            let (agent, reasoning) = self.agent_manager.resume_from(
                id,
                true,
                Arc::clone(&permission) as Arc<_>,
                reasoning_effort,
                Vec::new(),
            )?;
            (id, agent, reasoning)
        } else {
            let id = self.agent_id_allocator.next();
            let persistence = match self.persistence {
                SessionPersistence::Persistent => PersistenceConfig::Persistent,
                SessionPersistence::Ephemeral => PersistenceConfig::InMemory,
            };
            let kind = crate::agent::baked::root_kind();
            let (agent, reasoning) = self.agent_manager.create(
                id,
                kind,
                true,
                Arc::clone(&permission) as Arc<_>,
                reasoning_effort,
                persistence,
                Vec::new(),
            )?;
            self.state.get_mut().root_agent = Some(id);
            (id, agent, reasoning)
        };
        let previous = self
            .agents
            .insert(id, AgentSlot::new(id, agent, reasoning_handle));
        debug_assert!(previous.is_none());
        Ok(())
    }

    fn handle_agent_output(&mut self, output: crate::scheduler::AgentRunOutput<Http, Timer>) {
        let agent = output.agent;
        let is_root = self.root_id() == Some(agent);
        let Some(slot) = self.agents.get_mut(&agent) else {
            return;
        };
        let update = slot.accept_output(output);
        if !is_root {
            return;
        }
        match update {
            AgentSlotUpdate::Event(Ok(event)) => self.handle_agent_event(event),
            AgentSlotUpdate::Event(Err(error)) => {
                self.emit_turn_error(error.into());
                self.finish_turn();
            }
            AgentSlotUpdate::Returned => self.finish_turn(),
            AgentSlotUpdate::Reaped => {
                self.finish_turn();
            }
            AgentSlotUpdate::Ignored => {}
        }
    }

    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted { origin } => {
                let origin = match origin {
                    AgentTurnOrigin::Message => TurnOrigin::User,
                    AgentTurnOrigin::ToolCall { call } => TurnOrigin::ToolCall { call },
                };
                self.begin_turn(origin);
            }
            AgentEvent::Iteration(progress) => self.emit_iteration(progress),
            AgentEvent::InputRequired(AgentInputRequest::Approval {
                tool_call_id,
                tool_call,
                reason,
            }) => self.request_approval(tool_call_id, tool_call, reason),
            AgentEvent::TurnEnded { outcome } => {
                if let AgentOutcome::Completed(AgentCompletion::Synthesized(message)) = outcome {
                    self.emit_text(message);
                }
                self.approval.cancel();
                self.finish_turn();
            }
        }
    }

    fn emit_iteration(&mut self, progress: StreamPart<AgentIterationEvent>) {
        let event = match progress {
            StreamPart::Delta(AgentIterationEvent::Started(iteration)) => {
                IterationEvent::Started { iteration }
            }
            StreamPart::Delta(AgentIterationEvent::Reasoning(part)) => {
                IterationEvent::Reasoning(part)
            }
            StreamPart::Delta(AgentIterationEvent::Output(part)) => IterationEvent::Output(part),
            #[cfg(feature = "cache_profile")]
            StreamPart::Delta(AgentIterationEvent::Usage(usage)) => IterationEvent::Usage { usage },
            StreamPart::Delta(AgentIterationEvent::ToolResult(part)) => {
                IterationEvent::ToolResult(part)
            }
            StreamPart::End => IterationEvent::Ended,
        };
        self.emit_turn(TurnEvent::Iteration(event));
    }

    fn request_approval(&mut self, tool_call_id: ToolCallId, tool_call: ToolCall, reason: String) {
        let (request, kind) = self.approval.request(tool_call_id, tool_call, reason);
        self.emit_turn(TurnEvent::InputRequested { request, kind });
    }

    fn handle_approval_result(&mut self, completion: ApprovalCompletion) {
        let (request, tool_call_id, result) = completion.into_parts();
        let decision = match result {
            Ok(decision) => decision,
            Err(error) => {
                let rejection = format!("approval resolution failed: {error}");
                self.emit_input_error(request, error.into());
                ApprovalDecision::Rejected(rejection)
            }
        };
        let resolution = self
            .root()
            .ok_or(crate::agent::AgentApprovalError::NotAwaitingApproval)
            .and_then(|root| root.resolve_approval(tool_call_id, decision));
        if let Err(error) = resolution {
            self.emit_input_error(request, error.into());
            if let Some(root) = self.root_mut() {
                root.cancel();
            }
        }
    }

    fn begin_turn(&mut self, origin: TurnOrigin) {
        debug_assert!(self.active_turn.is_none());
        let turn = TurnId(self.next_turn);
        self.next_turn = self.next_turn.saturating_add(1);
        self.active_turn = Some(turn);
        self.emit_turn(TurnEvent::Started { turn, origin });
    }

    fn finish_turn(&mut self) {
        let Some(turn) = self.active_turn.take() else {
            return;
        };
        self.emit_turn(TurnEvent::Ended { turn });
    }

    fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        if self.state.get().reasoning_effort != effort {
            self.state.get_mut().reasoning_effort = effort;
        }
        for agent in self.agents.values() {
            agent.set_reasoning_effort(effort);
        }
    }

    fn root_id(&self) -> Option<crate::agent::AgentId> {
        self.state.get().root_agent
    }

    fn root(&self) -> Option<&AgentSlot<Http, Timer>> {
        let root = self.root_id()?;
        self.agents.get(&root)
    }

    fn root_mut(&mut self) -> Option<&mut AgentSlot<Http, Timer>> {
        let root = self.root_id()?;
        self.agents.get_mut(&root)
    }

    fn accepts(&self, lease: u64) -> bool {
        self.lifecycle.is_running()
            && self
                .client
                .as_ref()
                .is_some_and(|client| client.lease == lease)
    }

    fn reject_closed(&self, ack: Sender<Result<(), SessionControlError>>) {
        let _ = ack.try_send(Err(SessionControlError::SessionClosed(self.session)));
    }

    fn emit_text(&self, message: String) {
        self.emit_turn(TurnEvent::Output(StreamPart::Delta(message)));
        self.emit_turn(TurnEvent::Output(StreamPart::End));
    }

    fn emit_turn_error(&self, source: SessionTurnError) {
        self.emit_turn(TurnEvent::Error(TurnEventError::Execution(source)));
    }

    fn emit_input_error(&self, request: InputRequestId, source: SessionInputError) {
        self.emit_turn(TurnEvent::Error(TurnEventError::InputResolutionFailed {
            request,
            source,
        }));
    }

    fn emit_event_error(&self, error: SessionEventError) {
        tracing::error!(
            name: "session_execution_error",
            error = %error,
        );
        self.emit(SessionEvent::Error(error));
    }

    fn emit_turn(&self, event: TurnEvent) {
        self.emit(SessionEvent::Turn(event));
    }

    fn emit_closed(&mut self, reason: SessionCloseReason) {
        if let Some(client) = self.client.take() {
            let _ = client.events.try_send(SessionEvent::Closed(reason));
        }
    }

    fn emit(&self, event: SessionEvent) {
        if let Some(client) = &self.client {
            let _ = client.events.try_send(event);
        }
    }
}

pub(super) enum SessionActorStatus {
    Progress,
    Exit(SessionActorExit),
}

#[cfg(test)]
mod tests {
    use super::StopReason;

    #[test]
    fn stop_reason_escalation_is_explicit_and_monotonic() {
        assert_eq!(
            StopReason::Close.escalate(StopReason::Shutdown),
            StopReason::Shutdown
        );
        assert_eq!(
            StopReason::Shutdown.escalate(StopReason::Close),
            StopReason::Shutdown
        );
        assert_eq!(
            StopReason::Shutdown.escalate(StopReason::Delete),
            StopReason::Delete
        );
        assert_eq!(
            StopReason::Delete.escalate(StopReason::Close),
            StopReason::Delete
        );
    }
}

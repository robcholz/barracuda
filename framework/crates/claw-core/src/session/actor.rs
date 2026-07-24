#[cfg(feature = "multiagent")]
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
#[cfg(feature = "multiagent")]
use std::collections::BTreeMap;
use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use async_channel::{Receiver, Sender};
use claw_api::ToolCall;
use claw_interface::http::StreamingHttp;
#[cfg(feature = "multiagent")]
use claw_interface::Cancel;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_persistence::DurableState;
use claw_utils::stream::StreamPart;
use futures_core::Stream;

use super::agent_slot::{AgentDispatch, AgentSlot, AgentSlotUpdate, AgentSlots};
use super::approval::{
    ApprovalCompletion, ApprovalDisplay, ApprovalFlow, ApprovalRespondError, LlmApprovalResolver,
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
#[cfg(feature = "multiagent")]
use crate::agent::AgentDispatchError;
use crate::agent::{
    AgentCompletion, AgentCreateError, AgentError, AgentEvent, AgentId, AgentInputRequest,
    AgentIterationEvent, AgentOutcome, AgentTurnOrigin, ApprovalDecision, PersistenceConfig,
    ReasoningEffort, ToolCallId,
};
#[cfg(feature = "multiagent")]
use crate::multiagent::{
    DispatchOutcome, Multiagent, MultiagentEffect, MultiagentEffectResult, MultiagentPhysicalError,
    SubagentTimeout,
};
#[cfg(feature = "multiagent")]
type TimeoutFuture = Pin<Box<dyn Future<Output = crate::agent::AgentId>>>;

enum RootDispatchError {
    Retry(Message),
    Invariant,
}

#[cfg(feature = "multiagent")]
struct TimeoutEntry {
    timeout: SubagentTimeout,
    future: TimeoutFuture,
}

#[cfg(feature = "multiagent")]
#[derive(Default)]
struct AgentTimeouts {
    entries: BTreeMap<crate::agent::AgentId, TimeoutEntry>,
}

#[cfg(feature = "multiagent")]
impl AgentTimeouts {
    fn arm<Timer>(&mut self, agent: crate::agent::AgentId, timeout: SubagentTimeout)
    where
        Timer: ClawTimer + Default + 'static,
    {
        let future = Box::pin(async move {
            let mut timer = Timer::default();
            let _ = timer.sleep(timeout.duration(), Cancel::never()).await;
            agent
        });
        self.entries.insert(agent, TimeoutEntry { timeout, future });
    }

    fn remove(&mut self, agent: crate::agent::AgentId) {
        self.entries.remove(&agent);
    }

    fn poll_expired(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Option<(crate::agent::AgentId, SubagentTimeout)>> {
        let expired = self.entries.iter_mut().find_map(|(&agent, entry)| {
            entry
                .future
                .as_mut()
                .poll(context)
                .is_ready()
                .then_some((agent, entry.timeout))
        });
        let Some((agent, timeout)) = expired else {
            return Poll::Pending;
        };
        self.entries.remove(&agent);
        Poll::Ready(Some((agent, timeout)))
    }
}

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
            Self::DeleteReady(stopping) => {
                stopping.close_acks.extend(close_ack);
                if stopping.delete_ack.is_none() {
                    stopping.delete_ack = delete_ack;
                }
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
    #[cfg(feature = "multiagent")]
    Multiagent,
    #[cfg(feature = "multiagent")]
    Timeout,
}

impl PollSource {
    fn next(self) -> Self {
        match self {
            Self::Command => Self::Approval,
            Self::Approval => Self::Agent,
            #[cfg(feature = "multiagent")]
            Self::Agent => Self::Multiagent,
            #[cfg(not(feature = "multiagent"))]
            Self::Agent => Self::Command,
            #[cfg(feature = "multiagent")]
            Self::Multiagent => Self::Timeout,
            #[cfg(feature = "multiagent")]
            Self::Timeout => Self::Command,
        }
    }
}

#[cfg(feature = "multiagent")]
const POLL_SOURCE_COUNT: usize = 5;
#[cfg(not(feature = "multiagent"))]
const POLL_SOURCE_COUNT: usize = 3;

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
/// The actor polls its active Agents fairly and projects only root-Agent events
/// onto the public Session stream.
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
    #[cfg(feature = "multiagent")]
    multiagent: Multiagent,
    #[cfg(feature = "multiagent")]
    timeouts: AgentTimeouts,
    #[cfg(feature = "multiagent")]
    multiagent_reaping: BTreeSet<crate::agent::AgentId>,
    managed_agents: BTreeSet<crate::agent::AgentId>,

    active_agent_poll_queue: VecDeque<AgentId>,
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
    pub(super) fn new(
        session: SessionId,
        persistence: SessionPersistence,
        agent_manager: SharedAgentManager<Filesystem, Http, Timer>,
        agent_id_allocator: AgentIdAllocatorHandle,
        state: DurableState<SessionPersistentState>,
        approval_resolver: SharedApprovalResolver<Http, Timer>,
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
                #[cfg(feature = "multiagent")]
                multiagent: Multiagent::new(),
                #[cfg(feature = "multiagent")]
                timeouts: AgentTimeouts::default(),
                #[cfg(feature = "multiagent")]
                multiagent_reaping: BTreeSet::new(),
                managed_agents: BTreeSet::new(),
                active_agent_poll_queue: VecDeque::new(),
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

        for _ in 0..POLL_SOURCE_COUNT {
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
                    if let Poll::Ready((agent, update)) = self.poll_agents(context) {
                        self.handle_agent_output(agent, update);
                        return Poll::Ready(SessionActorStatus::Progress);
                    }
                }
                #[cfg(feature = "multiagent")]
                PollSource::Multiagent => {
                    if let Poll::Ready(effect) = self.multiagent.poll_effect(context) {
                        if let Some(effect) = effect {
                            self.handle_multiagent_effect(effect);
                        }
                        return Poll::Ready(SessionActorStatus::Progress);
                    }
                }
                #[cfg(feature = "multiagent")]
                PollSource::Timeout => {
                    if let Poll::Ready(Some((agent, _timeout))) =
                        self.timeouts.poll_expired(context)
                    {
                        self.multiagent.timeout(agent);
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

    /// Advance at most one ready Agent, rotating the queue after every poll.
    fn poll_agents(&mut self, context: &mut Context<'_>) -> Poll<(AgentId, AgentSlotUpdate)> {
        let active_count = self.active_agent_poll_queue.len();
        for _ in 0..active_count {
            let Some(agent) = self.active_agent_poll_queue.pop_front() else {
                break;
            };
            let Some(slot) = self.agents.get_mut(&agent) else {
                continue;
            };
            match slot.poll(context) {
                Poll::Ready(update) => {
                    if slot.is_in_flight() {
                        self.active_agent_poll_queue.push_back(agent);
                    }
                    return Poll::Ready((agent, update));
                }
                Poll::Pending => {
                    self.active_agent_poll_queue.push_back(agent);
                }
            }
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
        if let Some(root) = self.root_id() {
            if let Some(display) = self.approval.cancel_agent(root) {
                self.emit_approval_display(display);
            }
        }
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
        if reaping {
            for agent in self.agents.values_mut() {
                agent.begin_reaping();
            }
            return;
        }
        if let Some(root) = self.root_mut() {
            root.cancel();
        }
        #[cfg(feature = "multiagent")]
        self.multiagent.cleanup_subagents();
    }

    fn finish_lifecycle(&mut self) -> Option<SessionActorExit> {
        let reason = self.lifecycle.reason()?;
        if self.agents.values().any(AgentSlot::is_in_flight) {
            return None;
        }
        #[cfg(feature = "multiagent")]
        if reason != StopReason::Delete && !self.multiagent.root_children().is_empty() {
            return None;
        }
        self.finish_turn();

        let lifecycle = std::mem::replace(&mut self.lifecycle, ActorLifecycle::Running);
        let ActorLifecycle::Stopping(stopping) = lifecycle else {
            self.lifecycle = lifecycle;
            return None;
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
        let lifecycle = std::mem::replace(&mut self.lifecycle, ActorLifecycle::Running);
        let ActorLifecycle::DeleteReady(mut stopping) = lifecycle else {
            self.lifecycle = lifecycle;
            return false;
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
            self.begin_turn(TurnOrigin::User);
            self.finish_turn();
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
            Err(RootDispatchError::Retry(message)) => {
                self.inbox.push_front(message);
                false
            }
            Err(RootDispatchError::Invariant) => {
                self.begin_turn(TurnOrigin::User);
                self.emit_turn_error(SessionTurnError::Agent(AgentError::StateInvariant));
                self.finish_turn();
                true
            }
        }
    }

    fn dispatch_root(&mut self, message: Message) -> Result<(), RootDispatchError> {
        let Some(agent) = self.root_id() else {
            return Err(RootDispatchError::Invariant);
        };
        let turn = self.active_turn.unwrap_or(TurnId(self.next_turn));
        let span = agent_span(agent, Some(turn));
        let Some(root) = self.agents.get_mut(&agent) else {
            return Err(RootDispatchError::Invariant);
        };
        let dispatch = root
            .dispatch(message, span)
            .map_err(|(message, _)| RootDispatchError::Retry(message))?;
        if dispatch == AgentDispatch::Started {
            self.active_agent_poll_queue.push_back(agent);
        }
        Ok(())
    }

    fn delete_agents(&mut self) -> Result<(), AgentCreateError> {
        let root_agent = self.root_id();
        let mut agent_ids = self.agents.keys().copied().collect::<BTreeSet<_>>();
        agent_ids.extend(root_agent);
        agent_ids.extend(self.managed_agents.iter().copied());
        #[cfg(feature = "multiagent")]
        agent_ids.extend(self.multiagent.agent_ids());
        // Drop every live component handle before deleting its canonical
        // stores. In particular, dropping a filesystem TranscriptStore after
        // deletion could otherwise recreate its index file.
        self.agents.clear();
        self.active_agent_poll_queue.clear();
        let mut first_error = None;
        for agent in agent_ids {
            match self.agent_manager.remove(agent) {
                Ok(()) => {
                    if Some(agent) == root_agent {
                        self.state.get_mut().clear_root();
                    }
                }
                Err(error) => {
                    log::error!(
                        "session {} Agent {agent} delete failed: {error}",
                        self.session
                    );
                    tracing::error!(name: "session_agent_delete_failed", agent = %agent, error = %error);
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        #[cfg(feature = "multiagent")]
        self.multiagent.clear();
        first_error.map_or(Ok(()), Err)
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
        let (id, kind, agent, reasoning_handle, fresh) = if let Some(id) = root_agent {
            let kind = crate::agent::baked::root_kind().clone();
            #[cfg(feature = "multiagent")]
            let extension_tools = self.multiagent.tool_group(id, &kind).into_iter().collect();
            #[cfg(not(feature = "multiagent"))]
            let extension_tools = Vec::new();
            let (agent, reasoning) = self.agent_manager.resume_from(
                id,
                true,
                Arc::clone(&permission) as Arc<_>,
                reasoning_effort,
                extension_tools,
            )?;
            (id, kind, agent, reasoning, false)
        } else {
            let id = self.agent_id_allocator.next();
            let persistence = match self.persistence {
                SessionPersistence::Persistent => PersistenceConfig::Persistent,
                SessionPersistence::Ephemeral => PersistenceConfig::InMemory,
            };
            let kind = crate::agent::baked::root_kind().clone();
            #[cfg(feature = "multiagent")]
            let extension_tools = self.multiagent.tool_group(id, &kind).into_iter().collect();
            #[cfg(not(feature = "multiagent"))]
            let extension_tools = Vec::new();
            let (agent, reasoning) = self.agent_manager.create(
                id,
                &kind,
                true,
                Arc::clone(&permission) as Arc<_>,
                reasoning_effort,
                persistence,
                extension_tools,
            )?;
            (id, kind, agent, reasoning, true)
        };
        let previous = self
            .agents
            .insert(id, AgentSlot::new(agent, reasoning_handle));
        debug_assert!(previous.is_none());
        self.managed_agents.insert(id);
        #[cfg(feature = "multiagent")]
        if !self.multiagent.register_root(id, kind) {
            self.agents.remove(&id);
            if fresh {
                self.agent_manager.remove(id)?;
            }
            return Err(AgentCreateError::AgentAlreadyExists(id));
        }
        #[cfg(not(feature = "multiagent"))]
        let _ = kind;
        if fresh {
            self.state.get_mut().root_agent = Some(id);
        }
        Ok(())
    }

    #[cfg(feature = "multiagent")]
    fn handle_multiagent_effect(&mut self, effect: MultiagentEffect) {
        match effect {
            MultiagentEffect::Spawn { requester, command } => {
                self.spawn_subagent(requester, command);
            }
            MultiagentEffect::Dispatch {
                target,
                message,
                purpose,
            } => {
                let retry = message.clone();
                let turn = if self.root_id() == Some(target) {
                    Some(self.active_turn.unwrap_or(TurnId(self.next_turn)))
                } else {
                    self.active_turn
                };
                let dispatch = match self.agents.get_mut(&target) {
                    Some(slot) => {
                        let span = agent_span(target, turn);
                        Some(slot.dispatch(message, span))
                    }
                    None => None,
                };
                let outcome = match dispatch {
                    Some(Ok(started)) => {
                        if started == AgentDispatch::Started {
                            self.active_agent_poll_queue.push_back(target);
                        }
                        DispatchOutcome::Accepted
                    }
                    Some(Err((_, AgentDispatchError::Busy))) => DispatchOutcome::Busy,
                    Some(Err((_, AgentDispatchError::Closed))) | None => DispatchOutcome::Missing,
                };
                self.multiagent
                    .apply_result(MultiagentEffectResult::Dispatched {
                        target,
                        message: retry,
                        purpose,
                        outcome,
                    });
            }
            MultiagentEffect::RemoveAgents { agents } => {
                self.begin_multiagent_removal(agents);
            }
            MultiagentEffect::ArmTimeout { agent, timeout } => {
                if self.multiagent.contains(agent) {
                    self.timeouts.arm::<Timer>(agent, timeout);
                }
            }
        }
    }

    #[cfg(feature = "multiagent")]
    fn spawn_subagent(
        &mut self,
        requester: crate::agent::AgentId,
        command: crate::multiagent::SpawnCommand,
    ) {
        let id = self.agent_id_allocator.next();
        let kind = command.spec.kind().clone();
        let permission = Arc::new(SessionPermission::new(self.state.clone()));
        let extension_tools = self.multiagent.tool_group(id, &kind).into_iter().collect();
        let reasoning_effort = self.state.get().reasoning_effort;
        match self.agent_manager.create(
            id,
            &kind,
            false,
            permission as Arc<_>,
            reasoning_effort,
            PersistenceConfig::InMemory,
            extension_tools,
        ) {
            Ok((agent, reasoning)) => {
                let previous = self.agents.insert(id, AgentSlot::new(agent, reasoning));
                debug_assert!(previous.is_none());
                self.managed_agents.insert(id);
                let rollback = self
                    .multiagent
                    .apply_result(MultiagentEffectResult::Spawned {
                        requester,
                        command,
                        id,
                    });
                if let Some(rollback) = rollback {
                    self.rollback_spawn(rollback);
                }
            }
            Err(error) => {
                self.multiagent
                    .apply_result(MultiagentEffectResult::SpawnFailed {
                        command,
                        detail: error.to_string(),
                    });
            }
        }
    }

    #[cfg(feature = "multiagent")]
    fn rollback_spawn(&mut self, id: crate::agent::AgentId) {
        if self.agents.get(&id).is_some_and(AgentSlot::is_in_flight) {
            if let Some(slot) = self.agents.get_mut(&id) {
                slot.begin_reaping();
            }
            self.multiagent_reaping.insert(id);
            return;
        }
        self.agents.remove(&id);
        if let Err(error) = self.agent_manager.remove(id) {
            log::error!(
                "session {} Agent {id} spawn rollback failed: {error}",
                self.session
            );
            tracing::error!(name: "spawn_rollback_failed", agent = %id, error = %error);
        }
    }

    #[cfg(feature = "multiagent")]
    fn begin_multiagent_removal(&mut self, agents: Vec<crate::agent::AgentId>) {
        let victims = agents.iter().copied().collect::<BTreeSet<_>>();
        if let Some(display) = self.approval.cancel_agents(&victims) {
            self.emit_approval_display(display);
        }
        for agent in agents {
            self.timeouts.remove(agent);
            let in_flight = self.agents.get(&agent).is_some_and(AgentSlot::is_in_flight);
            if in_flight {
                if let Some(slot) = self.agents.get_mut(&agent) {
                    slot.begin_reaping();
                }
                self.multiagent_reaping.insert(agent);
                continue;
            }

            self.agents.remove(&agent);
            let result = self
                .agent_manager
                .remove(agent)
                .map_err(|error| MultiagentPhysicalError::new(error.to_string()));
            self.multiagent.physical_agent_removed(agent, result);
        }
    }

    fn handle_agent_output(&mut self, agent: AgentId, update: AgentSlotUpdate) {
        let is_root = self.root_id() == Some(agent);
        match update {
            AgentSlotUpdate::Event(Ok(event)) => {
                self.handle_agent_event(agent, event);
            }
            AgentSlotUpdate::Event(Err(error)) => {
                if is_root {
                    self.emit_turn_error(error.into());
                    self.finish_turn();
                    #[cfg(feature = "multiagent")]
                    self.multiagent.on_agent_idle(agent);
                } else {
                    #[cfg(feature = "multiagent")]
                    self.multiagent
                        .on_agent_completed(agent, format!("[failed: {error}]"), false);
                }
            }
            AgentSlotUpdate::Returned => {
                #[cfg(feature = "multiagent")]
                self.multiagent.on_agent_idle(agent);
            }
            AgentSlotUpdate::Reaped => {
                self.finish_reaped_agent(agent);
            }
            AgentSlotUpdate::Ignored => {}
        }
        #[cfg(feature = "multiagent")]
        self.drain_ready_multiagent_effects();
    }

    #[cfg(feature = "multiagent")]
    fn drain_ready_multiagent_effects(&mut self) {
        loop {
            let effect = self.multiagent.take_effect();
            let Some(effect) = effect else {
                break;
            };
            self.handle_multiagent_effect(effect);
        }
    }

    fn finish_reaped_agent(&mut self, agent: crate::agent::AgentId) {
        self.agents.remove(&agent);
        self.active_agent_poll_queue
            .retain(|queued| *queued != agent);
        let result = self.agent_manager.remove(agent);
        #[cfg(feature = "multiagent")]
        {
            let result = result.map_err(|error| MultiagentPhysicalError::new(error.to_string()));
            if self.multiagent_reaping.remove(&agent) {
                self.multiagent.physical_agent_removed(agent, result);
            } else if let Err(error) = result {
                log::error!(
                    "session {} Agent {agent} cleanup failed: {error}",
                    self.session
                );
                tracing::error!(name: "agent_cleanup_failed", agent = %agent, error = %error);
            }
        }
        #[cfg(not(feature = "multiagent"))]
        if let Err(error) = result {
            log::error!(
                "session {} Agent {agent} cleanup failed: {error}",
                self.session
            );
            tracing::error!(name: "agent_cleanup_failed", agent = %agent, error = %error);
        }
    }

    fn handle_agent_event(&mut self, agent: crate::agent::AgentId, event: AgentEvent) {
        let is_root = self.root_id() == Some(agent);
        match event {
            AgentEvent::TurnStarted { origin } => {
                #[cfg(feature = "multiagent")]
                self.multiagent.on_agent_started(agent);
                if !is_root {
                    return;
                }
                let origin = match origin {
                    AgentTurnOrigin::Message => TurnOrigin::User,
                    AgentTurnOrigin::ToolCall { call } => TurnOrigin::ToolCall { call },
                };
                if self.active_turn.is_none() {
                    self.begin_turn(origin);
                }
            }
            AgentEvent::Iteration(progress) => {
                if is_root {
                    self.emit_iteration(progress);
                }
            }
            AgentEvent::InputRequired(AgentInputRequest::Approval {
                tool_call_id,
                tool_call,
                reason,
            }) => {
                #[cfg(feature = "multiagent")]
                self.multiagent.on_agent_awaiting_approval(agent);
                self.request_approval(agent, tool_call_id, tool_call, reason);
            }
            AgentEvent::TurnEnded { outcome } => {
                let (_text, _completed, _cancelled) = match outcome {
                    AgentOutcome::Completed(AgentCompletion::Synthesized(message)) => {
                        if is_root {
                            self.emit_text(message.clone());
                        }
                        (message, true, false)
                    }
                    AgentOutcome::Completed(AgentCompletion::Streamed(message)) => {
                        (message, true, false)
                    }
                    AgentOutcome::Interrupted => {
                        ("subagent was interrupted".to_owned(), false, false)
                    }
                    AgentOutcome::Cancelled => ("subagent was cancelled".to_owned(), false, true),
                };
                #[cfg(feature = "multiagent")]
                if _cancelled {
                    self.multiagent.on_agent_cancelled(agent);
                } else {
                    self.multiagent.on_agent_completed(agent, _text, _completed);
                }
                if let Some(display) = self.approval.cancel_agent(agent) {
                    self.emit_approval_display(display);
                }
                if is_root {
                    self.finish_turn();
                }
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

    fn request_approval(
        &mut self,
        agent: crate::agent::AgentId,
        tool_call_id: ToolCallId,
        tool_call: ToolCall,
        reason: String,
    ) {
        if let Some(display) = self
            .approval
            .request(agent, tool_call_id, tool_call, reason)
        {
            self.emit_approval_display(display);
        }
    }

    fn handle_approval_result(&mut self, completion: ApprovalCompletion) {
        let (agent, request, tool_call_id, result) = completion.into_parts();
        let decision = match result {
            Ok(decision) => decision,
            Err(error) => {
                let rejection = format!("approval resolution failed: {error}");
                self.emit_input_error(request, error.into());
                ApprovalDecision::Rejected(rejection)
            }
        };
        let resolution = self
            .agents
            .get(&agent)
            .ok_or(crate::agent::AgentApprovalError::NotAwaitingApproval)
            .and_then(|slot| slot.resolve_approval(tool_call_id, decision));
        if let Err(error) = resolution {
            self.emit_input_error(request, error.into());
            if let Some(slot) = self.agents.get_mut(&agent) {
                slot.cancel();
            }
        } else {
            #[cfg(feature = "multiagent")]
            self.multiagent.on_approval_resolved(agent);
        }
        if let Some(display) = self.approval.activate_next() {
            self.emit_approval_display(display);
        }
    }

    fn emit_approval_display(&self, display: ApprovalDisplay) {
        self.emit_turn(TurnEvent::InputRequested {
            request: display.request,
            kind: display.kind,
        });
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
        log::error!("session {} execution failed: {error}", self.session);
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

fn agent_span(agent: crate::agent::AgentId, turn: Option<TurnId>) -> tracing::Span {
    match turn {
        Some(turn) => tracing::info_span!(
            "agent",
            trace.task = %agent,
            run.turn = %turn,
            run.agent = %agent,
        ),
        None => tracing::info_span!(
            "agent",
            trace.task = %agent,
            run.agent = %agent,
        ),
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

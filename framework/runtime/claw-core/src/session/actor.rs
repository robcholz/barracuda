use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::VecDeque;
use std::sync::Arc;

use async_channel::{Receiver, Sender};
use claw_api::ChatStreamEvent;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_persistence::DurableState;
use claw_utils::stream::StreamPart;
use futures_core::Stream;
use tracing::Instrument as _;

use crate::agent::{
    AdditionalAgentState, AgentCompletion, AgentCreateError, AgentEvent, AgentId, AgentInputRequest,
    AgentOutcome, IterationEvent as AgentIterationEvent, PersistenceConfig, ReasoningEffort,
    ToolCallId,
};
use crate::config::SharedApiManager;
use crate::scheduler::{agent_run_route, AgentRunReceiver, AgentRunRoute, AgentRunSchedulerHandle};
use claw_api::ToolCall;

use super::agent_slot::{AgentSlot, AgentSlotUpdate};
use super::api::{OpenSessionError, SessionControlError};
use super::approval_resolver::{
    self, ApprovalControl, ApprovalResolverError, PermissionReplyResolution,
};
use super::command::{ControlOp, SessionCommand};
use super::manager::SharedAgentManager;
use super::permission_policy::SessionPermission;
use super::state::{next_agent, SessionManagerState, SessionPersistentState};
use super::{
    InputRequestId, InputRequestKind, IterationEvent, Message, SessionCloseReason, SessionError,
    SessionEvent, SessionEventError, SessionId, SessionInputError, SessionPersistence,
    SessionTurnError, TurnEvent, TurnEventError, TurnId, TurnOrigin,
};

type ApprovalFuture =
    Pin<Box<dyn Future<Output = Result<PermissionReplyResolution, ApprovalResolverError>>>>;

struct PendingApproval {
    request: InputRequestId,
    tool_call_id: ToolCallId,
    tool_call: ToolCall,
    reason: String,
}

struct ApprovalTask {
    control: ApprovalControl,
    future: ApprovalFuture,
    request: InputRequestId,
    tool_call_id: ToolCallId,
    tool_call: ToolCall,
    reason: String,
}

struct ActiveTurn {
    id: TurnId,
    toolcalls: Vec<ToolCall>,
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
    Deleted { session: SessionId },
    Shutdown {
        session: SessionId,
    },
}

impl SessionActorExit {
    pub(super) fn session(&self) -> SessionId {
        match self {
            Self::Deleted { session } | Self::Shutdown { session } => *session,
        }
    }
}

/// One long-lived Session stream backed by one queued root Agent.
///
/// The actor never polls BaseAgent directly. It moves the resident Agent into
/// the process-global Scheduler and only reduces scheduler outputs back into
/// the slot and outward Session stream.
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
    permission: Arc<SessionPermission>,
    api_manager: SharedApiManager,

    root: Option<AgentSlot<Http, Timer>>,
    restored_root: Option<AgentId>,
    inbox: VecDeque<Message>,
    active_turn: Option<ActiveTurn>,
    next_turn: u32,
    next_input_request: u32,
    pending_approval: Option<PendingApproval>,
    approval: Option<ApprovalTask>,

    scheduler: AgentRunSchedulerHandle<Http, Timer>,
    run_route: AgentRunRoute<Http, Timer>,
    run_outputs: AgentRunReceiver<Http, Timer>,
    commands: Pin<Box<Receiver<SessionCommand>>>,
    next_source: PollSource,

    events: Option<Sender<Result<SessionEvent, SessionError>>>,
    active_lease: Option<u64>,
    next_lease: u64,
    close_requested: bool,
    close_acks: Vec<Sender<Result<(), SessionControlError>>>,
    delete_requested: bool,
    shutdown_requested: bool,
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
        state: DurableState<SessionPersistentState>,
        api_manager: SharedApiManager,
        scheduler: AgentRunSchedulerHandle<Http, Timer>,
    ) -> (Self, Sender<SessionCommand>) {
        let (command_sender, commands) = async_channel::unbounded();
        let (run_route, run_outputs) = agent_run_route();
        let restored_root = (persistence == SessionPersistence::Persistent)
            .then(|| state.get().root_agent)
            .flatten();
        let permission = Arc::new(SessionPermission::new(state.clone()));
        (
            Self {
                session,
                persistence,
                state,
                agent_manager,
                permission,
                api_manager,
                root: None,
                restored_root,
                inbox: VecDeque::new(),
                active_turn: None,
                next_turn: 1,
                next_input_request: 1,
                pending_approval: None,
                approval: None,
                scheduler,
                run_route,
                run_outputs,
                commands: Box::pin(commands),
                next_source: PollSource::Command,
                events: None,
                active_lease: None,
                next_lease: 1,
                close_requested: false,
                close_acks: Vec::new(),
                delete_requested: false,
                shutdown_requested: false,
            },
            command_sender,
        )
    }

    pub(super) fn poll(
        &mut self,
        context: &mut Context<'_>,
        manager_state: &DurableState<SessionManagerState>,
    ) -> Poll<SessionActorStatus> {
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
                    if let Some(task) = &mut self.approval {
                        if let Poll::Ready(result) = task.future.as_mut().poll(context) {
                            let task = self
                                .approval
                                .take()
                                .expect("a ready approval task remains installed");
                            self.handle_approval_result(task, result);
                            return Poll::Ready(SessionActorStatus::Progress);
                        }
                    }
                }
                PollSource::Agent => {
                    if let Poll::Ready(Some(output)) =
                        Pin::new(&mut self.run_outputs).poll_next(context)
                    {
                        self.handle_agent_output(output);
                        return Poll::Ready(SessionActorStatus::Progress);
                    }
                }
            }
        }

        if self.start_next_message(manager_state) {
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

    pub(super) fn open(
        &mut self,
        events: Sender<Result<SessionEvent, SessionError>>,
    ) -> Result<u64, OpenSessionError> {
        if self.active_lease.is_some()
            || self.close_requested
            || self.delete_requested
            || self.shutdown_requested
        {
            return Err(OpenSessionError::AlreadyOpen(self.session));
        }
        let lease = self.next_lease;
        self.next_lease = self.next_lease.saturating_add(1);
        self.active_lease = Some(lease);
        self.events = Some(events);
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
        self.inbox.push_back(message.into_user());
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
        if self.approval.is_some() {
            let _ = ack.try_send(Err(SessionControlError::NotAwaitingInput(self.session)));
            return;
        }
        let Some(pending) = self.pending_approval.take() else {
            let _ = ack.try_send(Err(SessionControlError::NotAwaitingInput(self.session)));
            return;
        };
        if pending.request != request {
            let expected = pending.request;
            self.pending_approval = Some(pending);
            let _ = ack.try_send(Err(SessionControlError::InputRequestMismatch {
                session: self.session,
                expected,
                received: request,
            }));
            return;
        }

        let control = ApprovalControl::new();
        let task_control = control.clone();
        let api_manager = Arc::clone(&self.api_manager);
        let tool_call = pending.tool_call.clone();
        let reason = pending.reason.clone();
        let user_reply = message.as_str().to_owned();
        let future = Box::pin(
            async move {
                approval_resolver::resolve_permission_reply::<Http, Timer>(
                    &api_manager,
                    &tool_call,
                    &reason,
                    &user_reply,
                    &task_control,
                )
                .await
            }
            .instrument(tracing::info_span!("approval.resolve")),
        );
        self.approval = Some(ApprovalTask {
            control,
            future,
            request: pending.request,
            tool_call_id: pending.tool_call_id,
            tool_call: pending.tool_call,
            reason: pending.reason,
        });
        let _ = ack.try_send(Ok(()));
    }

    fn control(&mut self, lease: u64, op: ControlOp, ack: Sender<Result<(), SessionControlError>>) {
        if !self.accepts(lease) {
            self.reject_closed(ack);
            return;
        }
        match op {
            ControlOp::Interrupt => {
                if let Some(root) = &mut self.root {
                    root.interrupt();
                }
            }
            ControlOp::Cancel => {
                if let Some(root) = &mut self.root {
                    root.cancel();
                }
            }
        }
        if let Some(approval) = &self.approval {
            approval.control.cancel();
        }
        self.pending_approval = None;
        let _ = ack.try_send(Ok(()));
    }

    fn close(&mut self, lease: u64, ack: Sender<Result<(), SessionControlError>>) {
        if !self.accepts(lease) {
            self.reject_closed(ack);
            return;
        }
        self.close_requested = true;
        self.close_acks.push(ack);
        self.stop_current_run(false);
    }

    pub(super) fn request_delete(&mut self) {
        self.delete_requested = true;
        self.stop_current_run(true);
    }

    pub(super) fn request_shutdown(&mut self) {
        self.shutdown_requested = true;
        self.stop_current_run(false);
    }

    fn stop_current_run(&mut self, reaping: bool) {
        self.inbox.clear();
        self.pending_approval = None;
        if let Some(approval) = self.approval.take() {
            approval.control.cancel();
        }
        if let Some(root) = &mut self.root {
            if reaping {
                root.begin_reaping();
            } else {
                root.cancel();
            }
        }
    }

    fn finish_lifecycle(&mut self) -> Option<SessionActorExit> {
        if !(self.close_requested || self.delete_requested || self.shutdown_requested) {
            return None;
        }
        if self.root.as_ref().is_some_and(AgentSlot::is_in_flight) {
            return None;
        }
        self.finish_turn();

        if self.delete_requested {
            if let Err(error) = self.delete_root_agent() {
                self.emit_event_error(SessionEventError::DeleteFailed { source: error });
                self.delete_requested = false;
                return None;
            }
            self.emit_closed(SessionCloseReason::Deleted);
            self.complete_close_requests();
            return Some(SessionActorExit::Deleted {
                session: self.session,
            });
        }
        if self.shutdown_requested {
            self.emit_closed(SessionCloseReason::RuntimeShutdown);
            self.complete_close_requests();
            return Some(SessionActorExit::Shutdown {
                session: self.session,
            });
        }

        self.emit_closed(SessionCloseReason::Requested);
        self.active_lease = None;
        self.close_requested = false;
        self.complete_close_requests();
        None
    }

    fn complete_close_requests(&mut self) {
        for ack in std::mem::take(&mut self.close_acks) {
            let _ = ack.try_send(Ok(()));
        }
    }

    fn start_next_message(&mut self, manager_state: &DurableState<SessionManagerState>) -> bool {
        if self.close_requested || self.delete_requested || self.shutdown_requested {
            return false;
        }
        if self.root.as_ref().is_some_and(AgentSlot::is_in_flight) || self.inbox.is_empty() {
            return false;
        }
        let message = self
            .inbox
            .pop_front()
            .expect("a checked non-empty inbox has a front message");
        self.begin_turn();
        if let Err(error) = self.ensure_root(manager_state) {
            self.emit_turn_error(error.into());
            self.finish_turn();
            return true;
        }
        if message.as_str().trim().is_empty() {
            self.finish_turn();
            return true;
        }
        let turn = self
            .active_turn
            .as_ref()
            .expect("begin_turn installs the active turn")
            .id;
        let root = self
            .root
            .as_mut()
            .expect("ensure_root materializes a root slot");
        let agent = root.id();
        let span = tracing::info_span!(
            "agent",
            trace.task = %agent,
            run.agent = %agent,
            run.turn = %turn,
        );
        root.start(message, &self.scheduler, self.run_route.clone(), span);
        true
    }

    fn delete_root_agent(&mut self) -> Result<(), AgentCreateError> {
        let root_agent = self.root.as_ref().map(AgentSlot::id).or(self.restored_root);
        // Drop every live component handle before deleting its canonical
        // stores. In particular, dropping a filesystem TranscriptStore after
        // deletion could otherwise recreate its index file.
        self.root = None;
        if self.persistence == SessionPersistence::Persistent {
            if let Some(root_agent) = root_agent {
                if let Err(error) = self.agent_manager.remove(root_agent) {
                    self.restored_root = Some(root_agent);
                    return Err(error);
                }
            }
        }
        self.restored_root = None;
        self.state.get_mut().clear_root();
        Ok(())
    }

    fn ensure_root(
        &mut self,
        manager_state: &DurableState<SessionManagerState>,
    ) -> Result<(), AgentCreateError> {
        if self.root.is_some() {
            return Ok(());
        }
        let reasoning_effort = self.state.get().reasoning_effort;
        let (id, agent, reasoning_handle) = if let Some(id) = self.restored_root.take() {
            let root_inflight_toolcalls = self.state.get().root_inflight_toolcalls().to_vec();
            let additional = (!root_inflight_toolcalls.is_empty())
                .then(|| AdditionalAgentState::new(root_inflight_toolcalls));
            let (agent, reasoning) = self.agent_manager.resume_from(
                id,
                true,
                Arc::clone(&self.permission) as Arc<_>,
                reasoning_effort,
                Vec::new(),
                additional,
            )?;
            (id, agent, reasoning)
        } else {
            let id = next_agent(manager_state);
            let persistence = match self.persistence {
                SessionPersistence::Persistent => PersistenceConfig::Persistent,
                SessionPersistence::Ephemeral => PersistenceConfig::InMemory,
            };
            let kind = crate::agent::baked::root_kind();
            let (agent, reasoning) = self.agent_manager.create(
                id,
                kind,
                true,
                Arc::clone(&self.permission) as Arc<_>,
                reasoning_effort,
                persistence,
                Vec::new(),
            )?;
            self.state.get_mut().root_agent = Some(id);
            (id, agent, reasoning)
        };
        self.root = Some(AgentSlot::new(id, agent, reasoning_handle));
        Ok(())
    }

    fn handle_agent_output(&mut self, output: crate::scheduler::AgentRunOutput<Http, Timer>) {
        let Some(root) = &mut self.root else {
            return;
        };
        match root.accept_output(output) {
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
            AgentEvent::Iteration(StreamPart::Delta(AgentIterationEvent::BeforeToolCalls(
                calls,
            ))) => {
                self.record_toolcalls(calls);
            }
            AgentEvent::Iteration(progress) => self.emit_iteration(progress),
            AgentEvent::InputRequired(AgentInputRequest::Approval {
                tool_call_id,
                tool_call,
                reason,
            }) => self.request_approval(tool_call_id, tool_call, reason),
            AgentEvent::Finished(outcome) => {
                if let AgentOutcome::Completed(AgentCompletion::Synthesized(message)) = outcome {
                    self.emit_text(message);
                }
                self.pending_approval = None;
                self.approval = None;
                self.finish_turn();
            }
        }
    }

    fn emit_iteration(&self, progress: StreamPart<AgentIterationEvent>) {
        let event = match progress {
            StreamPart::Delta(AgentIterationEvent::Started(iteration)) => {
                IterationEvent::Started { iteration }
            }
            StreamPart::Delta(AgentIterationEvent::Llm(ChatStreamEvent::Reasoning(part))) => {
                IterationEvent::Reasoning(part)
            }
            StreamPart::Delta(AgentIterationEvent::Llm(ChatStreamEvent::Output(part))) => {
                IterationEvent::Output(part)
            }
            StreamPart::Delta(AgentIterationEvent::Llm(ChatStreamEvent::ToolCalls(part))) => {
                IterationEvent::ToolCalls(part)
            }
            StreamPart::Delta(AgentIterationEvent::BeforeToolCalls(_)) => return,
            StreamPart::End => IterationEvent::Ended,
        };
        self.emit_turn(TurnEvent::Iteration(event));
    }

    fn request_approval(&mut self, tool_call_id: ToolCallId, tool_call: ToolCall, reason: String) {
        let request = InputRequestId(self.next_input_request);
        self.next_input_request = self.next_input_request.saturating_add(1);
        let kind = InputRequestKind::PermissionApproval {
            tool_call: tool_call.clone(),
            reason: reason.clone(),
        };
        self.pending_approval = Some(PendingApproval {
            request,
            tool_call_id,
            tool_call,
            reason,
        });
        self.emit_turn(TurnEvent::InputRequested { request, kind });
    }

    fn handle_approval_result(
        &mut self,
        task: ApprovalTask,
        result: Result<PermissionReplyResolution, ApprovalResolverError>,
    ) {
        let resolution = match result {
            Ok(resolution) => resolution,
            Err(ApprovalResolverError::Cancelled) => return,
            Err(error) => {
                self.emit_input_error(task.request, error.into());
                self.request_approval(task.tool_call_id, task.tool_call, task.reason);
                return;
            }
        };
        let Some(decision) = resolution.clone().into_decision() else {
            let PermissionReplyResolution::Clarify(_) = resolution else {
                unreachable!("non-clarification resolutions map to decisions")
            };
            self.request_approval(task.tool_call_id, task.tool_call, task.reason);
            return;
        };
        let resolution = self
            .root
            .as_ref()
            .ok_or(crate::agent::AgentApprovalError::NotAwaitingApproval)
            .and_then(|root| root.resolve_approval(task.tool_call_id, decision));
        if let Err(error) = resolution {
            self.emit_input_error(task.request, error.into());
            if let Some(root) = &mut self.root {
                root.cancel();
            }
        }
    }

    fn begin_turn(&mut self) {
        debug_assert!(self.active_turn.is_none());
        let turn = TurnId(self.next_turn);
        self.next_turn = self.next_turn.saturating_add(1);
        self.active_turn = Some(ActiveTurn {
            id: turn,
            toolcalls: Vec::new(),
        });
        self.emit_turn(TurnEvent::Started {
            turn,
            origin: TurnOrigin::User,
        });
    }

    fn finish_turn(&mut self) {
        let Some(turn) = self.active_turn.take() else {
            return;
        };
        if !turn.toolcalls.is_empty() {
            let mut state = self.state.get_mut();
            for call in &turn.toolcalls {
                state.remove_root_inflight_toolcall(call);
            }
        }
        self.emit_turn(TurnEvent::Ended { turn: turn.id });
    }

    fn record_toolcalls(&mut self, calls: Vec<ToolCall>) {
        let Some(turn) = &mut self.active_turn else {
            return;
        };
        let mut state = self.state.get_mut();
        for call in calls {
            state.add_root_inflight_toolcall(&call);
            turn.toolcalls.push(call);
        }
    }

    fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        if self.state.get().reasoning_effort != effort {
            self.state.get_mut().reasoning_effort = effort;
        }
        if let Some(root) = &self.root {
            root.set_reasoning_effort(effort);
        }
    }

    fn accepts(&self, lease: u64) -> bool {
        self.active_lease == Some(lease)
            && !self.close_requested
            && !self.delete_requested
            && !self.shutdown_requested
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
        if self.events.is_some() {
            self.emit(SessionEvent::Closed(reason));
            self.events = None;
        }
    }

    fn emit(&self, event: SessionEvent) {
        if let Some(events) = &self.events {
            let _ = events.try_send(Ok(event));
        }
    }
}

pub(super) enum SessionActorStatus {
    Progress,
    Exit(SessionActorExit),
}

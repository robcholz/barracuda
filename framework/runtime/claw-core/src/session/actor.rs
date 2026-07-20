use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::rc::Rc;
use std::sync::{Arc, RwLock};

use async_channel::{Receiver, Sender};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_persistence::DurableState;
use futures_core::Stream;
use strum::IntoStaticStr;
use tracing::Instrument as _;

use crate::agent::{AgentResume, FsAgentFactory};
use crate::config::ClawApiManager;
use crate::multiagent::{
    AgentIdAllocator, ApprovalResolutionError, DriveControl, DriveOutcome, DriveOutput, DriveStop,
    MultiagentDeliverError, MultiagentRuntime, MultiagentState, MultiagentWork, TurnStopMode,
};
use crate::protocol::{
    EventSink, InputRequestId, InputRequestKind, Message, SessionEvent, SessionId,
    SessionPersistence, StreamPart, TrackedToolCall, TurnId, TurnOrigin,
};

use super::api::{
    ControlOp, OpenSessionError, SessionCommand, SessionControlError, SessionEndpoint,
};
use super::approval::{self, ApprovalResolverError, PermissionReplyResolution};
use super::permission::SessionPermission;
use super::persistence::SessionState;
use super::state::{PendingTurnInput, TurnState};

type RuntimeFuture<Filesystem, Http, Timer> =
    Pin<Box<dyn Future<Output = RuntimeCompletion<Filesystem, Http, Timer>>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeDriveKind {
    Foreground,
    Background,
    Stop,
}

impl RuntimeDriveKind {
    fn is_foreground(self) -> bool {
        self == Self::Foreground
    }
}

enum RuntimeDriveResult {
    Driven(Result<DriveOutcome, DeliverError>),
    Stopped,
}

struct RuntimeCompletion<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    runtime: Box<MultiagentRuntime<Filesystem, Http, Timer>>,
    result: RuntimeDriveResult,
}

enum RuntimeExecution<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    Idle(Box<MultiagentRuntime<Filesystem, Http, Timer>>),
    Driving {
        kind: RuntimeDriveKind,
        control: DriveControl,
        future: RuntimeFuture<Filesystem, Http, Timer>,
    },
}

#[derive(Debug, IntoStaticStr, thiserror::Error)]
enum DeliverError {
    #[strum(serialize = "agent")]
    #[error(transparent)]
    Multiagent(#[from] MultiagentDeliverError),
    #[strum(serialize = "agent")]
    #[error(transparent)]
    ApprovalResolver(#[from] ApprovalResolverError),
    #[strum(serialize = "agent")]
    #[error(transparent)]
    ApprovalResolution(#[from] ApprovalResolutionError),
}

pub(crate) enum SessionActorExit {
    Deleted(SessionId),
    Shutdown(SessionId),
}

impl SessionActorExit {
    pub(crate) fn session(&self) -> SessionId {
        match self {
            Self::Deleted(session) | Self::Shutdown(session) => *session,
        }
    }
}

/// The sole owner of one session's turn state, event stream, and agent graph.
pub(crate) struct SessionActor<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    session: SessionId,
    persistence: SessionPersistence,
    state: DurableState<SessionState>,
    /// Active turns and request ids are process-local and restart at boot.
    turn: TurnState,
    execution: Option<RuntimeExecution<Filesystem, Http, Timer>>,
    api_manager: Arc<RwLock<ClawApiManager>>,
    events: Option<EventSink>,
    active_lease: Option<u64>,
    next_lease: u64,
    announced_turn: Option<TurnId>,
    announced_input_request: Option<InputRequestId>,
    requested_control: Option<ControlOp>,
    control_acks: Vec<Sender<Result<(), SessionControlError>>>,
    close_requested: bool,
    close_acks: Vec<Sender<Result<(), SessionControlError>>>,
    delete_requested: bool,
    delete_acks: Vec<Sender<Result<(), SessionControlError>>>,
    shutdown_requested: bool,
}

impl<Filesystem, Http, Timer> SessionActor<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session: SessionId,
        persistence: SessionPersistence,
        factory: Rc<FsAgentFactory<Filesystem, Http, Timer>>,
        agent_ids: AgentIdAllocator,
        state: DurableState<SessionState>,
        api_manager: Arc<RwLock<ClawApiManager>>,
    ) -> Self {
        let (root_mode, recovery) = {
            let state = state.get();
            (state.mode(), state.recovery())
        };
        let resume = recovery.map(|recovery| {
            AgentResume::new(recovery.loaded_tool_groups, recovery.inflight_toolcalls)
        });
        let permission = Arc::new(SessionPermission::new(state.clone()));
        let runtime = MultiagentRuntime::new_with_resume(
            session,
            factory,
            agent_ids,
            permission.clone(),
            MultiagentState::default(),
            root_mode,
            resume,
        );
        Self {
            session,
            persistence,
            state,
            turn: TurnState::default(),
            execution: Some(RuntimeExecution::Idle(Box::new(runtime))),
            api_manager,
            events: None,
            active_lease: None,
            next_lease: 1,
            announced_turn: None,
            announced_input_request: None,
            requested_control: None,
            control_acks: Vec::new(),
            close_requested: false,
            close_acks: Vec::new(),
            delete_requested: false,
            delete_acks: Vec::new(),
            shutdown_requested: false,
        }
    }

    pub(crate) async fn run(mut self, commands: Receiver<SessionCommand>) -> SessionActorExit {
        let mut commands = Box::pin(commands);
        loop {
            if let Some(exit) = self.advance() {
                return exit;
            }

            match (ActorPoll {
                commands: commands.as_mut(),
                execution: &mut self.execution,
            })
            .await
            {
                ActorEvent::Command(Some(command)) => self.handle_command(command),
                ActorEvent::Command(None) => self.shutdown_requested = true,
                ActorEvent::RuntimeFinished { kind, result } => {
                    if self.handle_runtime_finished(kind, result) {
                        futures_lite::future::yield_now().await;
                    }
                }
                ActorEvent::RuntimeTimedOut { output } => self.handle_idle_timeout(output),
            }
        }
    }

    /// Run immediate state transitions until the actor must wait for a command
    /// or one runtime operation.
    fn advance(&mut self) -> Option<SessionActorExit> {
        loop {
            if self.is_driving() {
                return None;
            }

            if self.delete_requested || self.shutdown_requested || self.close_requested {
                if self.needs_stop() {
                    self.start_stop(TurnStopMode::DeleteSpawnedAgents);
                    return None;
                }
                self.finish_active_turn(false);
                self.finish_control_request();
                if self.delete_requested {
                    self.emit_closed();
                    for ack in std::mem::take(&mut self.delete_acks) {
                        let _ = ack.try_send(Ok(()));
                    }
                    return Some(SessionActorExit::Deleted(self.session));
                }
                if self.shutdown_requested {
                    self.record_recovery();
                    self.emit_closed();
                    return Some(SessionActorExit::Shutdown(self.session));
                }
                self.finish_close();
                continue;
            }

            if let Some(op) = self.requested_control {
                if self.needs_stop() {
                    let mode = match op {
                        ControlOp::Interrupt => TurnStopMode::PreserveAgents,
                        ControlOp::Cancel => TurnStopMode::DeleteSpawnedAgents,
                    };
                    self.start_stop(mode);
                    return None;
                }
                self.finish_active_turn(false);
                self.finish_control_request();
                continue;
            }

            self.active_lease?;

            self.ensure_input_request();

            self.announce_turn();
            self.announce_input_request();
            if let Some(input) = self.turn.take_pending_input() {
                self.start_turn_input(input);
                return None;
            }
            if self.turn.active_input_request().is_some() {
                return None;
            }

            match self.runtime().work() {
                MultiagentWork::Root => match self.turn.active_turn_origin() {
                    None => {
                        let origin = self
                            .runtime()
                            .pending_root_origin()
                            .expect("root work outside a turn has a subagent origin");
                        let effort = self.state.get().reasoning_effort();
                        self.turn.begin_subagent_turn(origin, effort);
                    }
                    Some(TurnOrigin::User) if self.runtime().pending_root_origin().is_some() => {
                        self.start_pending_root_result();
                        return None;
                    }
                    Some(TurnOrigin::Subagent { .. })
                        if self.runtime().pending_root_origin().is_some() =>
                    {
                        self.start_pending_root_result();
                        return None;
                    }
                    Some(TurnOrigin::User | TurnOrigin::Subagent { .. }) => {
                        self.start_root_resume();
                        return None;
                    }
                },
                MultiagentWork::Background => {
                    if self.turn.has_active_turn() {
                        self.finish_active_turn(true);
                    } else {
                        self.start_background();
                        return None;
                    }
                }
                MultiagentWork::None => {
                    if self.turn.has_active_turn() {
                        self.finish_active_turn(true);
                    } else {
                        return None;
                    }
                }
            }
        }
    }

    fn handle_command(&mut self, command: SessionCommand) {
        match command {
            SessionCommand::Open {
                events,
                commands,
                ack,
            } => self.open(events, commands, ack),
            SessionCommand::Submit {
                lease,
                message,
                ack,
            } => self.submit(lease, message, ack),
            SessionCommand::Respond {
                lease,
                request,
                message,
                ack,
            } => self.respond(lease, request, message, ack),
            SessionCommand::Control { lease, op, ack } => self.control(lease, op, ack),
            SessionCommand::SetReasoningEffort { lease, effort, ack } => {
                if self.accepts(lease) {
                    if self.state.get().reasoning_effort() != effort {
                        self.state.get_mut().set_reasoning_effort(effort);
                    }
                    let _ = ack.try_send(Ok(()));
                } else {
                    self.reject_closed(ack);
                }
            }
            SessionCommand::SetPermissionLevel { lease, level, ack } => {
                if self.accepts(lease) {
                    if self.state.get().permission_level() != level {
                        self.state.get_mut().set_permission_level(level);
                    }
                    let _ = ack.try_send(Ok(()));
                } else {
                    self.reject_closed(ack);
                }
            }
            SessionCommand::Close { lease, ack } => self.close(lease, ack),
            SessionCommand::Delete { ack } => {
                self.delete_requested = true;
                self.delete_acks.push(ack);
                self.cancel_running();
            }
            SessionCommand::Shutdown => {
                self.shutdown_requested = true;
                self.cancel_running();
            }
        }
    }

    fn open(
        &mut self,
        events: EventSink,
        commands: Sender<SessionCommand>,
        ack: Sender<Result<SessionEndpoint, OpenSessionError>>,
    ) {
        if self.active_lease.is_some()
            || self.close_requested
            || self.delete_requested
            || self.shutdown_requested
        {
            let _ = ack.try_send(Err(OpenSessionError::AlreadyOpen(self.session)));
            return;
        }
        let lease = self.next_lease;
        self.next_lease = self.next_lease.saturating_add(1);
        self.active_lease = Some(lease);
        self.events = Some(events);
        self.announced_turn = None;
        self.announced_input_request = None;
        let _ = ack.try_send(Ok(SessionEndpoint::new(lease, commands)));
    }

    fn submit(&mut self, lease: u64, input: Message, ack: Sender<Result<(), SessionControlError>>) {
        let has_text = !input.as_str().is_empty();
        let text_bytes = input.as_str().len() as u64;
        if !self.accepts(lease) {
            tracing::warn!(name: "submit_rejected", reason = "session_closed");
            self.reject_closed(ack);
            return;
        }
        let foreground_running = self
            .driving_kind()
            .is_some_and(RuntimeDriveKind::is_foreground);
        let root_busy = self
            .idle_runtime()
            .is_some_and(|runtime| runtime.work() == MultiagentWork::Root);
        if foreground_running
            || self.turn.has_pending_input()
            || self.turn.has_active_turn()
            || root_busy
        {
            tracing::warn!(name: "submit_rejected", reason = "busy", has_text, text_bytes);
            let _ = ack.try_send(Err(SessionControlError::Busy(self.session)));
            return;
        }
        let effort = self.state.get().reasoning_effort();
        self.turn.begin_user_turn(input, effort);
        if let Some(control) = self.driving_control() {
            control.request_wake();
        }
        tracing::info!(name: "submit_accepted", has_text, text_bytes);
        let _ = ack.try_send(Ok(()));
    }

    fn respond(
        &mut self,
        lease: u64,
        request: InputRequestId,
        input: Message,
        ack: Sender<Result<(), SessionControlError>>,
    ) {
        if !self.accepts(lease) {
            tracing::warn!(name: "respond_rejected", reason = "session_closed");
            self.reject_closed(ack);
            return;
        }
        if self.is_driving() || self.turn.has_pending_input() {
            tracing::warn!(name: "respond_rejected", reason = "busy", request = %request);
            let _ = ack.try_send(Err(SessionControlError::Busy(self.session)));
            return;
        }
        let Some(expected) = self.turn.active_input_request().map(|pending| pending.id) else {
            tracing::warn!(name: "respond_rejected", reason = "not_awaiting_input", request = %request);
            let _ = ack.try_send(Err(SessionControlError::NotAwaitingInput(self.session)));
            return;
        };
        if expected != request {
            tracing::warn!(
                name: "respond_rejected",
                reason = "input_request_mismatch",
                expected = %expected,
                received = %request,
            );
            let _ = ack.try_send(Err(SessionControlError::InputRequestMismatch {
                session: self.session,
                expected,
                received: request,
            }));
            return;
        }
        let accepted = self.turn.respond_to_input(request, input);
        debug_assert!(accepted, "validated input request must still be active");
        self.announced_input_request = None;
        tracing::info!(name: "respond_accepted", request = %request);
        let _ = ack.try_send(Ok(()));
    }

    fn control(&mut self, lease: u64, op: ControlOp, ack: Sender<Result<(), SessionControlError>>) {
        if !self.accepts(lease) {
            self.reject_closed(ack);
            return;
        }
        let has_active_turn = self.turn.has_active_turn();
        let has_background = self
            .driving_kind()
            .is_some_and(|kind| kind == RuntimeDriveKind::Background)
            || self
                .idle_runtime()
                .is_some_and(|runtime| runtime.work() == MultiagentWork::Background);
        if !has_active_turn && (op == ControlOp::Interrupt || !has_background) {
            let _ = ack.try_send(Ok(()));
            return;
        }
        self.requested_control = Some(ControlOp::merge(self.requested_control, op));
        self.control_acks.push(ack);
        if let Some(control) = self.driving_control() {
            match self.requested_control {
                Some(ControlOp::Interrupt) => control.request_interrupt(),
                Some(ControlOp::Cancel) => control.request_cancel(),
                None => {}
            }
        }
    }

    fn close(&mut self, lease: u64, ack: Sender<Result<(), SessionControlError>>) {
        if !self.accepts(lease) {
            self.reject_closed(ack);
            return;
        }
        self.close_requested = true;
        self.close_acks.push(ack);
        self.cancel_running();
    }

    fn accepts(&self, lease: u64) -> bool {
        self.active_lease == Some(lease)
            && !self.delete_requested
            && !self.shutdown_requested
            && !self.close_requested
    }

    fn reject_closed(&self, ack: Sender<Result<(), SessionControlError>>) {
        let _ = ack.try_send(Err(SessionControlError::SessionClosed(self.session)));
    }

    fn needs_stop(&self) -> bool {
        self.turn.has_active_turn()
            || self
                .idle_runtime()
                .is_some_and(|runtime| runtime.work() != MultiagentWork::None)
    }

    fn cancel_running(&self) {
        if let Some(control) = self.driving_control() {
            control.request_cancel();
        }
    }

    fn announce_turn(&mut self) {
        let Some(turn) = self.turn.active_turn_id() else {
            return;
        };
        if self.announced_turn == Some(turn) {
            return;
        }
        let origin = self
            .turn
            .active_turn_origin()
            .expect("an active turn has an origin");
        self.announced_turn = Some(turn);
        self.emit(SessionEvent::TurnStarted { turn, origin });
    }

    fn ensure_input_request(&mut self) {
        if self.turn.has_pending_input() || self.turn.active_input_request().is_some() {
            return;
        }
        let Some(request) = self.runtime().required_input() else {
            return;
        };
        let (idle_origin, kind) = request.into_parts();
        let effort = self.state.get().reasoning_effort();
        let _ = self.turn.request_input(idle_origin, kind, effort);
    }

    fn announce_input_request(&mut self) {
        let Some(pending) = self.turn.active_input_request().cloned() else {
            return;
        };
        if self.announced_input_request == Some(pending.id) {
            return;
        }
        self.announced_input_request = Some(pending.id);
        self.emit(SessionEvent::InputRequested {
            request: pending.id,
            kind: pending.kind,
        });
    }

    fn finish_active_turn(&mut self, commit_background_results: bool) {
        let Some(finished) = self.turn.finish_turn() else {
            return;
        };
        if commit_background_results {
            let background_results = self.runtime_mut().commit_root_deliveries();
            self.settle_persisted_toolcalls(&background_results);
            let deferred = self.runtime().active_root_background_spawns();
            let ordinary = finished
                .toolcalls
                .into_iter()
                .filter(|call| !deferred.contains(call))
                .collect::<Vec<_>>();
            self.settle_persisted_toolcalls(&ordinary);
        }
        self.announced_turn = None;
        self.announced_input_request = None;
        self.record_recovery();
        self.emit(SessionEvent::TurnEnded { turn: finished.id });
    }

    fn record_tool_started(&mut self, call: TrackedToolCall) {
        self.state.get_mut().add_inflight_toolcall(&call);
        self.turn.record_tool_started(call);
    }

    fn settle_persisted_toolcalls(&self, calls: &[TrackedToolCall]) {
        if calls.is_empty() {
            return;
        }
        let mut state = self.state.get_mut();
        for call in calls {
            state.remove_inflight_toolcall(call);
        }
    }

    fn finish_control_request(&mut self) {
        self.requested_control = None;
        for ack in std::mem::take(&mut self.control_acks) {
            let _ = ack.try_send(Ok(()));
        }
    }

    fn finish_close(&mut self) {
        self.record_recovery();
        self.emit_closed();
        self.active_lease = None;
        self.close_requested = false;
        self.requested_control = None;
        for ack in std::mem::take(&mut self.control_acks) {
            let _ = ack.try_send(Ok(()));
        }
        for ack in std::mem::take(&mut self.close_acks) {
            let _ = ack.try_send(Ok(()));
        }
    }

    fn emit_closed(&mut self) {
        if self.events.is_some() {
            self.emit(SessionEvent::Closed);
            self.events = None;
        }
    }

    fn emit(&self, event: SessionEvent) {
        if let Some(events) = &self.events {
            events.emit(event);
        }
    }

    fn emit_error(&self, message: String) {
        self.emit(SessionEvent::Error { message });
    }

    fn record_recovery(&mut self) {
        if self.runtime().root_resume_pending() {
            return;
        }
        let Some((mode, mut loaded_groups)) = self.runtime().root_recovery() else {
            return;
        };
        loaded_groups.sort_unstable();
        loaded_groups.dedup();
        if self.state.get().recovery_matches(mode, &loaded_groups) {
            return;
        }
        self.state.get_mut().record_recovery(mode, loaded_groups);
    }

    fn runtime(&self) -> &MultiagentRuntime<Filesystem, Http, Timer> {
        self.idle_runtime()
            .expect("session runtime is idle outside an actor drive")
    }

    fn runtime_mut(&mut self) -> &mut MultiagentRuntime<Filesystem, Http, Timer> {
        match self.execution.as_mut() {
            Some(RuntimeExecution::Idle(runtime)) => runtime,
            Some(RuntimeExecution::Driving { .. }) => {
                panic!("session runtime is driving outside an actor drive")
            }
            None => panic!("session runtime left in a transition state"),
        }
    }

    fn idle_runtime(&self) -> Option<&MultiagentRuntime<Filesystem, Http, Timer>> {
        match self.execution.as_ref()? {
            RuntimeExecution::Idle(runtime) => Some(runtime),
            RuntimeExecution::Driving { .. } => None,
        }
    }

    fn take_runtime(&mut self) -> Box<MultiagentRuntime<Filesystem, Http, Timer>> {
        match self.execution.take() {
            Some(RuntimeExecution::Idle(runtime)) => runtime,
            Some(driving @ RuntimeExecution::Driving { .. }) => {
                self.execution = Some(driving);
                panic!("session runtime is already driving")
            }
            None => panic!("session runtime left in a transition state"),
        }
    }

    fn is_driving(&self) -> bool {
        matches!(self.execution, Some(RuntimeExecution::Driving { .. }))
    }

    fn driving_kind(&self) -> Option<RuntimeDriveKind> {
        match self.execution.as_ref()? {
            RuntimeExecution::Driving { kind, .. } => Some(*kind),
            RuntimeExecution::Idle(_) => None,
        }
    }

    fn driving_control(&self) -> Option<&DriveControl> {
        match self.execution.as_ref()? {
            RuntimeExecution::Driving { control, .. } => Some(control),
            RuntimeExecution::Idle(_) => None,
        }
    }

    fn start_turn_input(&mut self, input: PendingTurnInput) {
        let runtime = self.take_runtime();
        let events = self.events.clone().unwrap_or_else(EventSink::disabled);
        let effort = self
            .turn
            .reasoning_effort()
            .expect("user input belongs to an active turn");
        let persistence = self.persistence;
        let api_manager = Arc::clone(&self.api_manager);
        let turn = self
            .turn
            .active_turn_id()
            .expect("user input belongs to an active turn");
        let control = DriveControl::new();
        let drive_control = control.clone();
        let future = Box::pin(
            async move {
                let mut runtime = runtime;
                let result = drive_turn_input(
                    &mut runtime,
                    input,
                    effort,
                    persistence,
                    &events,
                    &drive_control,
                    &api_manager,
                )
                .await;
                RuntimeCompletion { runtime, result }
            }
            .instrument(tracing::info_span!("turn", run.turn = %turn, cause = "user_submit")),
        );
        self.execution = Some(RuntimeExecution::Driving {
            kind: RuntimeDriveKind::Foreground,
            control,
            future,
        });
    }

    fn start_pending_root_result(&mut self) {
        let runtime = self.take_runtime();
        let events = self.events.clone().unwrap_or_else(EventSink::disabled);
        let effort = self
            .turn
            .reasoning_effort()
            .expect("a pending root result belongs to an active turn");
        let turn = self
            .turn
            .active_turn_id()
            .expect("a pending root result belongs to an active turn");
        let control = DriveControl::new();
        let drive_control = control.clone();
        let future = Box::pin(
            async move {
                let mut runtime = runtime;
                let result =
                    drive_pending_root_result(&mut runtime, effort, &events, &drive_control).await;
                RuntimeCompletion { runtime, result }
            }
            .instrument(tracing::info_span!("turn", run.turn = %turn, cause = "background_result")),
        );
        self.execution = Some(RuntimeExecution::Driving {
            kind: RuntimeDriveKind::Foreground,
            control,
            future,
        });
    }

    fn start_root_resume(&mut self) {
        let runtime = self.take_runtime();
        let events = self.events.clone().unwrap_or_else(EventSink::disabled);
        let turn = self
            .turn
            .active_turn_id()
            .expect("resumed root work belongs to an active turn");
        let control = DriveControl::new();
        let drive_control = control.clone();
        let future = Box::pin(
            async move {
                let mut runtime = runtime;
                let result = drive_root(&mut runtime, &drive_control, &events).await;
                RuntimeCompletion {
                    runtime,
                    result: RuntimeDriveResult::Driven(result),
                }
            }
            .instrument(tracing::info_span!("turn", run.turn = %turn, cause = "runtime_resume")),
        );
        self.execution = Some(RuntimeExecution::Driving {
            kind: RuntimeDriveKind::Foreground,
            control,
            future,
        });
    }

    fn start_background(&mut self) {
        let runtime = self.take_runtime();
        let events = self.events.clone().unwrap_or_else(EventSink::disabled);
        let control = DriveControl::new();
        let drive_control = control.clone();
        let future = Box::pin(async move {
            let mut runtime = runtime;
            let result = drive_background(&mut runtime, &events, &drive_control).await;
            RuntimeCompletion { runtime, result }
        });
        self.execution = Some(RuntimeExecution::Driving {
            kind: RuntimeDriveKind::Background,
            control,
            future,
        });
    }

    fn start_stop(&mut self, mode: TurnStopMode) {
        let runtime = self.take_runtime();
        let future = Box::pin(async move {
            let mut runtime = runtime;
            runtime.stop_turn_tasks(mode).await;
            RuntimeCompletion {
                runtime,
                result: RuntimeDriveResult::Stopped,
            }
        });
        self.execution = Some(RuntimeExecution::Driving {
            kind: RuntimeDriveKind::Stop,
            control: DriveControl::new(),
            future,
        });
    }

    fn handle_runtime_finished(
        &mut self,
        kind: RuntimeDriveKind,
        result: RuntimeDriveResult,
    ) -> bool {
        let mut yield_for_persistence = false;
        if let RuntimeDriveResult::Driven(result) = result {
            match result {
                Ok(DriveOutcome::ToolStarted(output, call)) => {
                    let _ = self
                        .emit_drive_result(Ok::<_, DeliverError>((output, DriveStop::Quiescent)));
                    self.record_tool_started(call);
                    yield_for_persistence = true;
                }
                Ok(DriveOutcome::Complete(output, stop)) => {
                    let stop = self.emit_drive_result(Ok::<_, DeliverError>((output, stop)));
                    if kind.is_foreground() && stop != DriveStop::Quiescent {
                        self.finish_active_turn(false);
                    }
                }
                Err(error) => {
                    let stop = self.emit_drive_result(Err(error));
                    if kind.is_foreground() && stop != DriveStop::Quiescent {
                        self.finish_active_turn(false);
                    }
                }
            }
        }
        if self.requested_control.is_some() || self.close_requested || self.delete_requested {
            self.finish_active_turn(false);
            self.finish_control_request();
            yield_for_persistence = false;
        }
        yield_for_persistence
    }

    fn handle_idle_timeout(&mut self, output: DriveOutput) {
        let _ = self.emit_drive_result(Ok::<_, DeliverError>((output, DriveStop::Quiescent)));
        let active_kind = self
            .turn
            .active_input_request()
            .map(|request| request.kind.clone());
        let request_is_still_current = active_kind.as_ref().is_some_and(|active_kind| {
            self.runtime()
                .required_input()
                .is_some_and(|required| required.kind() == active_kind)
        });
        if active_kind.is_some() && !request_is_still_current {
            if let Some(request) = self.turn.cancel_input_request() {
                self.announced_input_request = None;
                tracing::info!(name: "input_request_cancelled", request = %request, reason = "subagent_timeout");
            }
        }
    }

    fn emit_drive_result(
        &self,
        result: Result<(DriveOutput, DriveStop), DeliverError>,
    ) -> DriveStop {
        match result {
            Ok((output, stop)) => {
                let mut emitted = false;
                for text in output.into_messages() {
                    self.emit(SessionEvent::Output(StreamPart::Delta(text)));
                    emitted = true;
                }
                if emitted {
                    self.emit(SessionEvent::Output(StreamPart::End));
                }
                stop
            }
            Err(error) => {
                let kind: &'static str = (&error).into();
                tracing::error!(name: "error", kind);
                self.emit_error(error.to_string());
                DriveStop::Quiescent
            }
        }
    }
}

enum ActorEvent {
    Command(Option<SessionCommand>),
    RuntimeTimedOut {
        output: DriveOutput,
    },
    RuntimeFinished {
        kind: RuntimeDriveKind,
        result: RuntimeDriveResult,
    },
}

struct ActorPoll<'a, Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    commands: Pin<&'a mut Receiver<SessionCommand>>,
    execution: &'a mut Option<RuntimeExecution<Filesystem, Http, Timer>>,
}

impl<Filesystem, Http, Timer> Future for ActorPoll<'_, Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    type Output = ActorEvent;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Poll::Ready(command) = this.commands.as_mut().poll_next(context) {
            return Poll::Ready(ActorEvent::Command(command));
        }
        match this.execution.as_mut() {
            Some(RuntimeExecution::Idle(runtime)) => runtime
                .poll_expired_timeouts(context)
                .map(|output| ActorEvent::RuntimeTimedOut { output }),
            Some(RuntimeExecution::Driving { kind, future, .. }) => {
                let kind = *kind;
                let Poll::Ready(completion) = future.as_mut().poll(context) else {
                    return Poll::Pending;
                };
                *this.execution = Some(RuntimeExecution::Idle(completion.runtime));
                Poll::Ready(ActorEvent::RuntimeFinished {
                    kind,
                    result: completion.result,
                })
            }
            None => Poll::Pending,
        }
    }
}

async fn drive_turn_input<Filesystem, Http, Timer>(
    runtime: &mut MultiagentRuntime<Filesystem, Http, Timer>,
    input: PendingTurnInput,
    effort: crate::config::ReasoningEffort,
    persistence: SessionPersistence,
    events: &EventSink,
    control: &DriveControl,
    api_manager: &Arc<RwLock<ClawApiManager>>,
) -> RuntimeDriveResult
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    let result = match input {
        PendingTurnInput::Submit(input) => {
            match runtime
                .deliver(input, effort.context_block(), persistence)
                .map_err(DeliverError::from)
            {
                Ok(()) => drive_root(runtime, control, events).await,
                Err(error) => Err(error),
            }
        }
        PendingTurnInput::Response {
            kind: InputRequestKind::PermissionApproval { summary },
            message,
        } => {
            // The request may belong to a child while a foreground root tool is
            // still running. Approval resolution does not mutate agent context.
            resolve_pending_approval(
                runtime,
                &summary,
                message.as_str(),
                control,
                events,
                api_manager,
            )
            .await
        }
    };
    RuntimeDriveResult::Driven(result)
}

async fn drive_pending_root_result<Filesystem, Http, Timer>(
    runtime: &mut MultiagentRuntime<Filesystem, Http, Timer>,
    effort: crate::config::ReasoningEffort,
    events: &EventSink,
    control: &DriveControl,
) -> RuntimeDriveResult
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    let result = if runtime.activate_pending_root_results() {
        runtime
            .set_root_context_block(effort.context_block())
            .map_err(DeliverError::from)
    } else {
        debug_assert!(false, "subagent turn requires one pending root result");
        Ok(())
    };
    let result = match result {
        Ok(()) => drive_root(runtime, control, events).await,
        Err(error) => Err(error),
    };
    RuntimeDriveResult::Driven(result)
}

async fn drive_background<Filesystem, Http, Timer>(
    runtime: &mut MultiagentRuntime<Filesystem, Http, Timer>,
    events: &EventSink,
    control: &DriveControl,
) -> RuntimeDriveResult
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    let (output, stop) = runtime
        .drive_background_until_root_ready(control, events)
        .await;
    if let Some(mode) = stop_mode(stop) {
        runtime.stop_turn_tasks(mode).await;
    }
    RuntimeDriveResult::Driven(Ok(DriveOutcome::Complete(output, stop)))
}

async fn drive_root<Filesystem, Http, Timer>(
    runtime: &mut MultiagentRuntime<Filesystem, Http, Timer>,
    control: &DriveControl,
    events: &EventSink,
) -> Result<DriveOutcome, DeliverError>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    let outcome = runtime.drive_root_turn(control, events).await;
    if let DriveOutcome::Complete(_, stop) = &outcome {
        if let Some(mode) = stop_mode(*stop) {
            runtime.stop_turn_tasks(mode).await;
        }
    }
    Ok(outcome)
}

fn stop_mode(stop: DriveStop) -> Option<TurnStopMode> {
    match stop {
        DriveStop::Cancelled => Some(TurnStopMode::DeleteSpawnedAgents),
        DriveStop::Interrupted => Some(TurnStopMode::PreserveAgents),
        DriveStop::Quiescent | DriveStop::Woken => None,
    }
}

async fn resolve_pending_approval<Filesystem, Http, Timer>(
    runtime: &mut MultiagentRuntime<Filesystem, Http, Timer>,
    summary: &str,
    user_reply: &str,
    control: &DriveControl,
    events: &EventSink,
    api_manager: &Arc<RwLock<ClawApiManager>>,
) -> Result<DriveOutcome, DeliverError>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    let resolution = match approval::resolve_permission_reply::<Http, Timer>(
        api_manager,
        summary,
        user_reply,
        control,
    )
    .await
    {
        Ok(resolution) => resolution,
        Err(ApprovalResolverError::Cancelled) => {
            runtime
                .stop_turn_tasks(TurnStopMode::DeleteSpawnedAgents)
                .await;
            return Ok(DriveOutcome::Complete(
                DriveOutput::default(),
                DriveStop::Cancelled,
            ));
        }
        Err(error) => return Err(error.into()),
    };

    let Some(decision) = resolution.clone().into_decision() else {
        let PermissionReplyResolution::Clarify(message) = resolution else {
            unreachable!("non-clarification resolutions map to approval decisions")
        };
        tracing::info!(name: "approval_clarification", reason = %message);
        return Ok(DriveOutcome::Complete(
            DriveOutput::default(),
            DriveStop::Quiescent,
        ));
    };
    runtime.resolve_required_input(decision)?;
    drive_root(runtime, control, events).await
}

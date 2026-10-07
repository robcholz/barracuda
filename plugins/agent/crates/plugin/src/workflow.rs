//! Agent session Action registration and direct Event emission into Workflow.

mod action {
    //! Agent operations registered directly as Workflow Actions.

    use alloc::boxed::Box;
    use alloc::format;
    use alloc::rc::Rc;
    use alloc::string::String;
    use alloc::vec::Vec;

    use barracuda_agent_runtime::{
        AgentRuntime, InputRequestId, Message, OpenSessionError, PermissionLevel, ReasoningEffort,
        RuntimeError, SessionControlError, SessionCreateError, SessionDeleteError, SessionId,
        SessionPersistence,
    };
    use barracuda_workflow_plugin::{
        workflow_action_schema, WorkflowActionFuture, WorkflowActionHandler,
        WorkflowActionRegistration, WorkflowActionRegistry, WorkflowActionRegistryError,
        WorkflowActionSchema,
    };
    use serde::{Deserialize, Serialize};

    use super::SessionRegistry;

    /// Stable business failure codes retained from the former Agent JSON contracts.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum AgentActionError {
        /// A semantically invalid identifier or value was supplied.
        InvalidRequest,
        /// The Agent runtime worker stopped.
        WorkerStopped,
        /// Persistent state could not be initialized.
        Persistence,
        /// The requested session does not exist.
        SessionNotFound,
        /// Session deletion is already in progress.
        AlreadyDeleting,
        /// Persistent session state could not be deleted.
        Storage,
        /// Another caller already owns the session event subscription.
        AlreadyOpen,
        /// `session.open` has not established a control handle.
        SessionNotOpen,
        /// The open session lease has closed.
        SessionClosed,
        /// The active turn is not awaiting caller input.
        NotAwaitingInput,
        /// The response targets a different input request.
        InputRequestMismatch,
    }

    impl AgentActionError {
        const fn code(self) -> &'static str {
            match self {
                Self::InvalidRequest => "invalid_request",
                Self::WorkerStopped => "worker_stopped",
                Self::Persistence => "persistence",
                Self::SessionNotFound => "session_not_found",
                Self::AlreadyDeleting => "already_deleting",
                Self::Storage => "storage",
                Self::AlreadyOpen => "already_open",
                Self::SessionNotOpen => "session_not_open",
                Self::SessionClosed => "session_closed",
                Self::NotAwaitingInput => "not_awaiting_input",
                Self::InputRequestMismatch => "input_request_mismatch",
            }
        }
    }

    impl core::fmt::Display for AgentActionError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str(self.code())
        }
    }

    #[derive(Serialize)]
    struct ErrorResponse {
        error: AgentActionError,
    }

    #[derive(Serialize)]
    struct EmptyResponse {}

    #[derive(Serialize)]
    #[serde(untagged)]
    enum EmptyOrErrorResponse {
        Success(EmptyResponse),
        Error(ErrorResponse),
    }

    impl EmptyOrErrorResponse {
        const fn success() -> Self {
            Self::Success(EmptyResponse {})
        }

        const fn error(error: AgentActionError) -> Self {
            Self::Error(ErrorResponse { error })
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "snake_case")]
    enum Persistence {
        Persistent,
        Ephemeral,
    }

    impl From<Persistence> for SessionPersistence {
        fn from(value: Persistence) -> Self {
            match value {
                Persistence::Persistent => Self::Persistent,
                Persistence::Ephemeral => Self::Ephemeral,
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NewSessionRequest {
        persistence: Persistence,
    }

    #[derive(Serialize)]
    struct NewSessionSuccess {
        session: String,
    }

    #[derive(Serialize)]
    #[serde(untagged)]
    enum NewSessionResponse {
        Success(NewSessionSuccess),
        Error(ErrorResponse),
    }

    struct NewSessionAction {
        runtime: Rc<AgentRuntime>,
    }

    impl WorkflowActionHandler for NewSessionAction {
        type Request = NewSessionRequest;
        type Response = NewSessionResponse;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema!("session.new");

        fn invoke(
            &self,
            request: NewSessionRequest,
        ) -> WorkflowActionFuture<'_, NewSessionResponse> {
            let runtime = Rc::clone(&self.runtime);
            Box::pin(async move {
                let response = match runtime.new_session(request.persistence.into()).await {
                    Ok(session) => {
                        log::info!("Agent created session `{session}`");
                        NewSessionResponse::Success(NewSessionSuccess {
                            session: format!("{session}"),
                        })
                    }
                    Err(RuntimeError::SessionCreate(SessionCreateError::WorkerStopped)) => {
                        log::warn!("Agent failed to create session: runtime worker stopped");
                        NewSessionResponse::Error(ErrorResponse {
                            error: AgentActionError::WorkerStopped,
                        })
                    }
                    Err(error) => {
                        log::warn!("Agent failed to create persistent session: {error}");
                        NewSessionResponse::Error(ErrorResponse {
                            error: AgentActionError::Persistence,
                        })
                    }
                };
                Ok(response)
            })
        }
    }

    const fn all_sessions() -> usize {
        usize::MAX
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ListSessionsRequest {
        #[serde(default)]
        offset: usize,
        #[serde(default = "all_sessions")]
        limit: usize,
    }

    #[derive(Serialize)]
    struct SessionListPage {
        sessions: Vec<String>,
        next_offset: Option<usize>,
    }

    #[derive(Serialize)]
    #[serde(untagged)]
    enum ListSessionsResponse {
        Success(SessionListPage),
        Error(ErrorResponse),
    }

    struct ListSessionsAction {
        runtime: Rc<AgentRuntime>,
    }

    impl WorkflowActionHandler for ListSessionsAction {
        type Request = ListSessionsRequest;
        type Response = ListSessionsResponse;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema!("session.list");

        fn invoke(
            &self,
            request: ListSessionsRequest,
        ) -> WorkflowActionFuture<'_, ListSessionsResponse> {
            let runtime = Rc::clone(&self.runtime);
            Box::pin(async move {
                if request.limit == 0 {
                    return Ok(ListSessionsResponse::Error(ErrorResponse {
                        error: AgentActionError::InvalidRequest,
                    }));
                }
                let sessions = runtime.list_sessions().await;
                let end = request
                    .offset
                    .saturating_add(request.limit)
                    .min(sessions.len());
                let page = sessions.get(request.offset..end).unwrap_or(&[]);
                Ok(ListSessionsResponse::Success(SessionListPage {
                    sessions: page.iter().map(|session| format!("{session}")).collect(),
                    next_offset: (end < sessions.len()).then_some(end),
                }))
            })
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SessionRequest {
        session: String,
    }

    #[derive(Serialize)]
    struct OpenSessionSuccess {
        session: String,
        run: String,
    }

    #[derive(Serialize)]
    struct OpenSessionFailure {
        session: String,
        error: AgentActionError,
    }

    #[derive(Serialize)]
    #[serde(untagged)]
    enum OpenSessionResponse {
        Success(OpenSessionSuccess),
        Invalid(ErrorResponse),
        Failure(OpenSessionFailure),
    }

    struct OpenSessionAction {
        runtime: Rc<AgentRuntime>,
        sessions: SessionRegistry,
    }

    impl WorkflowActionHandler for OpenSessionAction {
        type Request = SessionRequest;
        type Response = OpenSessionResponse;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema!("session.open");

        fn invoke(&self, request: SessionRequest) -> WorkflowActionFuture<'_, OpenSessionResponse> {
            let runtime = Rc::clone(&self.runtime);
            let sessions = self.sessions.clone();
            Box::pin(async move {
                let session = match parse_session(&request.session) {
                    Ok(session) => session,
                    Err(error) => {
                        return Ok(OpenSessionResponse::Invalid(ErrorResponse { error }));
                    }
                };
                let response = match runtime.open_session(session).await {
                    Ok((control, events)) => {
                        let run = sessions.insert(session, control, events);
                        OpenSessionResponse::Success(OpenSessionSuccess {
                            session: request.session,
                            run: format!("run-{run}"),
                        })
                    }
                    Err(error) => OpenSessionResponse::Failure(OpenSessionFailure {
                        session: request.session,
                        error: map_open_error(error),
                    }),
                };
                Ok(response)
            })
        }
    }

    struct DeleteSessionAction {
        runtime: Rc<AgentRuntime>,
    }

    impl WorkflowActionHandler for DeleteSessionAction {
        type Request = SessionRequest;
        type Response = EmptyOrErrorResponse;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema!("session.delete");

        fn invoke(
            &self,
            request: SessionRequest,
        ) -> WorkflowActionFuture<'_, EmptyOrErrorResponse> {
            let runtime = Rc::clone(&self.runtime);
            Box::pin(async move {
                let session = match parse_session(&request.session) {
                    Ok(session) => session,
                    Err(error) => return Ok(EmptyOrErrorResponse::error(error)),
                };
                let response = match runtime.delete_session(session).await {
                    Ok(()) => EmptyOrErrorResponse::success(),
                    Err(SessionDeleteError::SessionNotFound(_)) => {
                        EmptyOrErrorResponse::error(AgentActionError::SessionNotFound)
                    }
                    Err(SessionDeleteError::AlreadyDeleting(_)) => {
                        EmptyOrErrorResponse::error(AgentActionError::AlreadyDeleting)
                    }
                    Err(SessionDeleteError::WorkerStopped) => {
                        EmptyOrErrorResponse::error(AgentActionError::WorkerStopped)
                    }
                    Err(_error) => EmptyOrErrorResponse::error(AgentActionError::Storage),
                };
                Ok(response)
            })
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AppendRequest {
        session: String,
        text: String,
    }

    struct AppendAction {
        sessions: SessionRegistry,
    }

    impl WorkflowActionHandler for AppendAction {
        type Request = AppendRequest;
        type Response = EmptyOrErrorResponse;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema!("session.append");

        fn invoke(&self, request: AppendRequest) -> WorkflowActionFuture<'_, EmptyOrErrorResponse> {
            let sessions = self.sessions.clone();
            Box::pin(async move {
                let session = match parse_session(&request.session) {
                    Ok(session) => session,
                    Err(error) => return Ok(EmptyOrErrorResponse::error(error)),
                };
                let Some(control) = sessions.get(session) else {
                    return Ok(EmptyOrErrorResponse::error(
                        AgentActionError::SessionNotOpen,
                    ));
                };
                let response = match control.append(Message::text(request.text)).await {
                    Ok(()) => EmptyOrErrorResponse::success(),
                    Err(error) => EmptyOrErrorResponse::error(map_control_error(error)),
                };
                Ok(response)
            })
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RespondRequest {
        session: String,
        request: String,
        text: String,
    }

    struct RespondAction {
        sessions: SessionRegistry,
    }

    impl WorkflowActionHandler for RespondAction {
        type Request = RespondRequest;
        type Response = EmptyOrErrorResponse;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema!("session.respond");

        fn invoke(
            &self,
            request: RespondRequest,
        ) -> WorkflowActionFuture<'_, EmptyOrErrorResponse> {
            let sessions = self.sessions.clone();
            Box::pin(async move {
                let session = match parse_session(&request.session) {
                    Ok(session) => session,
                    Err(error) => return Ok(EmptyOrErrorResponse::error(error)),
                };
                let input = match parse_input_request(&request.request) {
                    Ok(input) => input,
                    Err(error) => return Ok(EmptyOrErrorResponse::error(error)),
                };
                let Some(control) = sessions.get(session) else {
                    return Ok(EmptyOrErrorResponse::error(
                        AgentActionError::SessionNotOpen,
                    ));
                };
                let response = match control.respond(input, Message::text(request.text)).await {
                    Ok(()) => EmptyOrErrorResponse::success(),
                    Err(error) => EmptyOrErrorResponse::error(map_control_error(error)),
                };
                Ok(response)
            })
        }
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Effort {
        Low,
        Medium,
        High,
        Ultra,
    }

    impl From<Effort> for ReasoningEffort {
        fn from(value: Effort) -> Self {
            match value {
                Effort::Low => Self::Low,
                Effort::Medium => Self::Medium,
                Effort::High => Self::High,
                Effort::Ultra => Self::Ultra,
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SetReasoningEffortRequest {
        session: String,
        effort: Effort,
    }

    struct SetReasoningEffortAction {
        sessions: SessionRegistry,
    }

    impl WorkflowActionHandler for SetReasoningEffortAction {
        type Request = SetReasoningEffortRequest;
        type Response = EmptyOrErrorResponse;

        const SCHEMA: WorkflowActionSchema =
            workflow_action_schema!("session.set_reasoning_effort");

        fn invoke(
            &self,
            request: SetReasoningEffortRequest,
        ) -> WorkflowActionFuture<'_, EmptyOrErrorResponse> {
            let sessions = self.sessions.clone();
            Box::pin(async move {
                let response = with_session(&sessions, &request.session, |control| async move {
                    control.set_reasoning_effort(request.effort.into()).await
                })
                .await;
                Ok(response)
            })
        }
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Level {
        Deny,
        Ask,
        AllowAll,
    }

    impl From<Level> for PermissionLevel {
        fn from(value: Level) -> Self {
            match value {
                Level::Deny => Self::Deny,
                Level::Ask => Self::Ask,
                Level::AllowAll => Self::AllowAll,
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SetPermissionLevelRequest {
        session: String,
        level: Level,
    }

    struct SetPermissionLevelAction {
        sessions: SessionRegistry,
    }

    impl WorkflowActionHandler for SetPermissionLevelAction {
        type Request = SetPermissionLevelRequest;
        type Response = EmptyOrErrorResponse;

        const SCHEMA: WorkflowActionSchema =
            workflow_action_schema!("session.set_permission_level");

        fn invoke(
            &self,
            request: SetPermissionLevelRequest,
        ) -> WorkflowActionFuture<'_, EmptyOrErrorResponse> {
            let sessions = self.sessions.clone();
            Box::pin(async move {
                let response = with_session(&sessions, &request.session, |control| async move {
                    control.set_permission_level(request.level.into()).await
                })
                .await;
                Ok(response)
            })
        }
    }

    struct InterruptAction {
        sessions: SessionRegistry,
    }

    impl WorkflowActionHandler for InterruptAction {
        type Request = SessionRequest;
        type Response = EmptyOrErrorResponse;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema!("session.interrupt");

        fn invoke(
            &self,
            request: SessionRequest,
        ) -> WorkflowActionFuture<'_, EmptyOrErrorResponse> {
            let sessions = self.sessions.clone();
            Box::pin(async move {
                Ok(
                    with_session(&sessions, &request.session, |control| async move {
                        control.interrupt().await
                    })
                    .await,
                )
            })
        }
    }

    struct CancelAction {
        sessions: SessionRegistry,
    }

    impl WorkflowActionHandler for CancelAction {
        type Request = SessionRequest;
        type Response = EmptyOrErrorResponse;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema!("session.cancel");

        fn invoke(
            &self,
            request: SessionRequest,
        ) -> WorkflowActionFuture<'_, EmptyOrErrorResponse> {
            let sessions = self.sessions.clone();
            Box::pin(async move {
                Ok(
                    with_session(&sessions, &request.session, |control| async move {
                        control.cancel().await
                    })
                    .await,
                )
            })
        }
    }

    struct CloseAction {
        sessions: SessionRegistry,
    }

    impl WorkflowActionHandler for CloseAction {
        type Request = SessionRequest;
        type Response = EmptyOrErrorResponse;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema!("session.close");

        fn invoke(
            &self,
            request: SessionRequest,
        ) -> WorkflowActionFuture<'_, EmptyOrErrorResponse> {
            let sessions = self.sessions.clone();
            Box::pin(async move {
                Ok(
                    with_session(&sessions, &request.session, |control| async move {
                        control.close().await
                    })
                    .await,
                )
            })
        }
    }

    async fn with_session<Operation, Future>(
        sessions: &SessionRegistry,
        session: &str,
        operation: Operation,
    ) -> EmptyOrErrorResponse
    where
        Operation: FnOnce(barracuda_agent_runtime::SessionControl) -> Future,
        Future: core::future::Future<Output = Result<(), SessionControlError>>,
    {
        let session = match parse_session(session) {
            Ok(session) => session,
            Err(error) => return EmptyOrErrorResponse::error(error),
        };
        let Some(control) = sessions.get(session) else {
            return EmptyOrErrorResponse::error(AgentActionError::SessionNotOpen);
        };
        match operation(control).await {
            Ok(()) => EmptyOrErrorResponse::success(),
            Err(error) => EmptyOrErrorResponse::error(map_control_error(error)),
        }
    }

    /// Registers every Agent session operation as a Workflow Action.
    pub(crate) fn register_actions(
        actions: &WorkflowActionRegistry,
        runtime: Rc<AgentRuntime>,
        sessions: SessionRegistry,
    ) -> Result<Vec<WorkflowActionRegistration>, WorkflowActionRegistryError> {
        let registrations = alloc::vec![
            actions.add_action(NewSessionAction {
                runtime: Rc::clone(&runtime),
            })?,
            actions.add_action(ListSessionsAction {
                runtime: Rc::clone(&runtime),
            })?,
            actions.add_action(OpenSessionAction {
                runtime: Rc::clone(&runtime),
                sessions: sessions.clone(),
            })?,
            actions.add_action(DeleteSessionAction { runtime })?,
            actions.add_action(AppendAction {
                sessions: sessions.clone(),
            })?,
            actions.add_action(RespondAction {
                sessions: sessions.clone(),
            })?,
            actions.add_action(SetReasoningEffortAction {
                sessions: sessions.clone(),
            })?,
            actions.add_action(SetPermissionLevelAction {
                sessions: sessions.clone(),
            })?,
            actions.add_action(InterruptAction {
                sessions: sessions.clone(),
            })?,
            actions.add_action(CancelAction {
                sessions: sessions.clone(),
            })?,
            actions.add_action(CloseAction { sessions })?,
        ];
        Ok(registrations)
    }

    fn parse_session(value: &str) -> Result<SessionId, AgentActionError> {
        parse_prefixed(value, "session-").map(SessionId::new)
    }

    fn parse_input_request(value: &str) -> Result<InputRequestId, AgentActionError> {
        parse_prefixed(value, "input-").map(InputRequestId::new)
    }

    fn parse_prefixed(value: &str, prefix: &str) -> Result<u32, AgentActionError> {
        value
            .strip_prefix(prefix)
            .filter(|digits| !digits.is_empty())
            .and_then(|digits| digits.parse::<u32>().ok())
            .ok_or(AgentActionError::InvalidRequest)
    }

    fn map_open_error(error: RuntimeError) -> AgentActionError {
        match error {
            RuntimeError::OpenSession(OpenSessionError::SessionNotFound(_)) => {
                AgentActionError::SessionNotFound
            }
            RuntimeError::OpenSession(OpenSessionError::AlreadyOpen(_)) => {
                AgentActionError::AlreadyOpen
            }
            RuntimeError::OpenSession(OpenSessionError::WorkerStopped) => {
                AgentActionError::WorkerStopped
            }
            _ => AgentActionError::WorkerStopped,
        }
    }

    fn map_control_error(error: SessionControlError) -> AgentActionError {
        match error {
            SessionControlError::SessionClosed(_) => AgentActionError::SessionClosed,
            SessionControlError::NotAwaitingInput(_) => AgentActionError::NotAwaitingInput,
            SessionControlError::InputRequestMismatch { .. } => {
                AgentActionError::InputRequestMismatch
            }
            SessionControlError::WorkerStopped => AgentActionError::WorkerStopped,
        }
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::expect_used)]

        use barracuda_workflow_plugin::WorkflowActionHandler;
        use serde_json::json;

        use super::{
            parse_input_request, parse_session, AgentActionError, AppendAction, CancelAction,
            CloseAction, DeleteSessionAction, EmptyOrErrorResponse, InterruptAction,
            ListSessionsAction, NewSessionAction, OpenSessionAction, RespondAction,
            SetPermissionLevelAction, SetReasoningEffortAction,
        };

        #[test]
        fn parses_public_identifiers() {
            assert!(parse_session("session-12").is_ok());
            assert!(parse_input_request("input-9").is_ok());
            assert_eq!(
                parse_session("session-4294967296"),
                Err(AgentActionError::InvalidRequest)
            );
        }

        #[test]
        fn registers_the_session_workflow_action_addresses() {
            assert_eq!(
                [
                    NewSessionAction::SCHEMA.address(),
                    ListSessionsAction::SCHEMA.address(),
                    OpenSessionAction::SCHEMA.address(),
                    DeleteSessionAction::SCHEMA.address(),
                    AppendAction::SCHEMA.address(),
                    RespondAction::SCHEMA.address(),
                    SetReasoningEffortAction::SCHEMA.address(),
                    SetPermissionLevelAction::SCHEMA.address(),
                    InterruptAction::SCHEMA.address(),
                    CancelAction::SCHEMA.address(),
                    CloseAction::SCHEMA.address(),
                ],
                [
                    "session.new",
                    "session.list",
                    "session.open",
                    "session.delete",
                    "session.append",
                    "session.respond",
                    "session.set_reasoning_effort",
                    "session.set_permission_level",
                    "session.interrupt",
                    "session.cancel",
                    "session.close",
                ]
            );
        }

        #[test]
        fn action_schemas_accept_supported_request_sizes() {
            assert!(ListSessionsAction::SCHEMA
                .request()
                .validate(&json!({ "limit": 1_000_000 }))
                .is_ok());
            assert!(AppendAction::SCHEMA
                .request()
                .validate(&json!({
                    "session": "session-1",
                    "text": "x".repeat(4_096)
                }))
                .is_ok());
        }

        #[test]
        fn keeps_business_errors_in_the_workflow_response_document() {
            let response = serde_json::to_value(EmptyOrErrorResponse::error(
                AgentActionError::SessionNotOpen,
            ))
            .expect("serialize response");
            assert_eq!(response, json!({ "error": "session_not_open" }));
        }
    }
}

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_agent_runtime::{
    stream::StreamPart, AgentRuntime, InputRequestKind, IterationEvent, ProviderUsage,
    SessionCloseReason, SessionControl, SessionEvent, SessionId, SessionStream, TurnEvent,
    TurnOrigin,
};
use barracuda_json_writer::{write_value, Object, Sink};
use barracuda_workflow_plugin::{
    EmitError, Event, JsonText, WorkflowActionRegistration, WorkflowActionRegistry,
    WorkflowActionRegistryError, WorkflowService,
};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use futures_lite::{future, future::poll_fn, Stream};
use serde_json::{json, Map, Value};

use action::register_actions;

/// Stable identity for Agent session output consumed by Workflows.
pub struct SessionOutputEvent;

impl Event for SessionOutputEvent {
    const ID: &'static str = "session.event";
}

struct OpenedSession {
    control: SessionControl,
    events: SessionStream,
}

struct RegistryState {
    next_run: u32,
    sessions: BTreeMap<SessionId, OpenedSession>,
}

struct RegistryInner {
    state: RefCell<RegistryState>,
    /// Wakes the event loop when a session is added.
    changed: Signal<NoopRawMutex, ()>,
}

#[derive(Clone)]
pub(crate) struct SessionRegistry(Rc<RegistryInner>);

impl Default for SessionRegistry {
    fn default() -> Self {
        Self(Rc::new(RegistryInner {
            state: RefCell::new(RegistryState {
                next_run: 1,
                sessions: BTreeMap::new(),
            }),
            changed: Signal::new(),
        }))
    }
}

impl SessionRegistry {
    pub(crate) fn get(&self, session: SessionId) -> Option<SessionControl> {
        self.0
            .state
            .borrow()
            .sessions
            .get(&session)
            .map(|opened| opened.control.clone())
    }

    pub(crate) fn insert(
        &self,
        session: SessionId,
        control: SessionControl,
        events: SessionStream,
    ) -> u32 {
        let mut state = self.0.state.borrow_mut();
        let run = state.next_run;
        state.next_run = state.next_run.checked_add(1).unwrap_or(1);
        state
            .sessions
            .insert(session, OpenedSession { control, events });
        drop(state);
        self.0.changed.signal(());
        run
    }

    async fn next_event(&self) -> PendingEvent {
        loop {
            let event = poll_fn(|context| self.poll_event(context));
            let changed = async {
                self.0.changed.wait().await;
                None
            };
            if let Some(event) = future::or(event, changed).await {
                return event;
            }
        }
    }

    fn poll_event(&self, context: &mut Context<'_>) -> Poll<Option<PendingEvent>> {
        let mut state = self.0.state.borrow_mut();
        let mut ready = None;
        for (session, opened) in &mut state.sessions {
            match Stream::poll_next(Pin::new(&mut opened.events), context) {
                Poll::Ready(Some(Ok(event))) => {
                    let terminal = matches!(&event, SessionEvent::Closed(_));
                    ready = Some((*session, PendingSessionEvent::Runtime(event), terminal));
                    break;
                }
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => {
                    ready = Some((*session, PendingSessionEvent::StreamError, true));
                    break;
                }
                Poll::Pending => {}
            }
        }

        let Some((session, event, terminal)) = ready else {
            return Poll::Pending;
        };
        if terminal {
            state.sessions.remove(&session);
        }
        Poll::Ready(Some(PendingEvent { session, event }))
    }
}

struct PendingEvent {
    session: SessionId,
    event: PendingSessionEvent,
}

enum PendingSessionEvent {
    Runtime(SessionEvent),
    StreamError,
}

/// Agent adapter that registers Workflow Actions and forwards session Events.
pub struct AgentWorkflowAdapter {
    runtime: Rc<AgentRuntime>,
    sessions: SessionRegistry,
}

impl AgentWorkflowAdapter {
    /// Creates an Agent-to-Workflow adapter around one shared runtime handle.
    #[must_use]
    pub fn new(runtime: Rc<AgentRuntime>) -> Self {
        Self {
            runtime,
            sessions: SessionRegistry::default(),
        }
    }

    /// Adds all Agent session operations to the Workflow Action registry.
    pub fn register_actions(
        &self,
        actions: &WorkflowActionRegistry,
    ) -> Result<Vec<WorkflowActionRegistration>, WorkflowActionRegistryError> {
        register_actions(actions, Rc::clone(&self.runtime), self.sessions.clone())
    }

    /// Forwards open-session output directly to Workflow until emission fails.
    pub async fn forward_session_events(
        self,
        workflow: Rc<WorkflowService>,
    ) -> Result<(), AgentWorkflowError> {
        emit_session_events(self.sessions, workflow).await
    }
}

/// Failure while driving the Agent-to-Workflow adapter.
#[derive(Debug, thiserror::Error)]
pub enum AgentWorkflowError {
    /// The static session Event identity was invalid.
    #[error(transparent)]
    Emit(#[from] EmitError),
    /// The process emitted more session Events than its sequence can represent.
    #[error("Agent session Event sequence overflowed")]
    SequenceOverflow,
}

async fn emit_session_events(
    sessions: SessionRegistry,
    workflow: Rc<WorkflowService>,
) -> Result<(), AgentWorkflowError> {
    let mut sequence = 0_u64;
    loop {
        // Every text delta becomes an Event, so pause while earlier ones are
        // still being handled instead of queuing the whole stream.
        workflow.ready_for::<SessionOutputEvent>().await;
        let pending = sessions.next_event().await;
        sequence = emit_session_event(&workflow, pending.session, sequence, pending.event)?;
    }
}

fn emit_session_event(
    workflow: &WorkflowService,
    session: SessionId,
    sequence: u64,
    event: PendingSessionEvent,
) -> Result<u64, AgentWorkflowError> {
    let mut emitter = SessionEmitter {
        workflow,
        listening: workflow.has_listener::<SessionOutputEvent>(),
        session,
        sequence,
    };
    match event {
        PendingSessionEvent::Runtime(SessionEvent::Turn(event)) => emitter.emit_turn(event)?,
        PendingSessionEvent::Runtime(SessionEvent::Error(error)) => emitter.emit(
            "session_error",
            json!({ "message": error.to_string(), "message_truncated": false }),
        )?,
        PendingSessionEvent::Runtime(SessionEvent::Closed(reason)) => {
            emitter.emit("closed", json!({ "reason": close_reason(reason) }))?;
        }
        PendingSessionEvent::StreamError => {
            emitter.emit("stream_error", json!({ "error": "worker_stopped" }))?;
        }
    }
    Ok(emitter.sequence)
}

struct SessionEmitter<'a> {
    workflow: &'a WorkflowService,
    /// Whether any Workflow runs for session output; Events nobody would
    /// handle are not built, though they still consume a sequence number.
    listening: bool,
    session: SessionId,
    sequence: u64,
}

impl SessionEmitter<'_> {
    fn emit_turn(&mut self, event: TurnEvent) -> Result<(), AgentWorkflowError> {
        match event {
            TurnEvent::Started { turn, origin } => {
                let turn = format!("{turn}");
                match origin {
                    TurnOrigin::User => {
                        self.emit("turn_started", json!({ "turn": turn, "origin": "user" }))?;
                    }
                    TurnOrigin::ToolCall { call } => {
                        self.emit(
                            "turn_started",
                            json!({ "turn": turn, "origin": "tool_call" }),
                        )?;
                        self.emit_text("turn_origin_tool_call_id_delta", call.id)?;
                        self.emit_text("turn_origin_tool_name_delta", call.name)?;
                        self.emit_text("turn_origin_arguments_delta", call.arguments_json)?;
                        self.emit("turn_origin_ended", json!({}))?;
                    }
                }
            }
            TurnEvent::InputRequested { request, kind } => match kind {
                InputRequestKind::PermissionApproval { tool_call, reason } => {
                    self.emit(
                        "input_request_started",
                        json!({
                            "request": format!("{request}"),
                            "kind": "permission_approval"
                        }),
                    )?;
                    self.emit_text("input_request_tool_call_id_delta", tool_call.id)?;
                    self.emit_text("input_request_tool_name_delta", tool_call.name)?;
                    self.emit_text("input_request_arguments_delta", tool_call.arguments_json)?;
                    self.emit_text("input_request_reason_delta", reason)?;
                    self.emit(
                        "input_requested",
                        json!({ "request": format!("{request}") }),
                    )?;
                }
            },
            TurnEvent::Iteration(event) => self.emit_iteration(event)?,
            TurnEvent::EffectOutput(StreamPart::Delta(text)) => {
                self.emit_text("effect_output_delta", text)?;
            }
            TurnEvent::EffectOutput(StreamPart::End) => {
                self.emit("effect_output_ended", json!({}))?;
            }
            TurnEvent::Error(error) => self.emit(
                "turn_error",
                json!({ "message": error.to_string(), "message_truncated": false }),
            )?,
            TurnEvent::Ended { turn } => {
                self.emit("turn_ended", json!({ "turn": format!("{turn}") }))?;
            }
        }
        Ok(())
    }

    fn emit_iteration(&mut self, event: IterationEvent) -> Result<(), AgentWorkflowError> {
        match event {
            IterationEvent::Started { iteration } => self.emit(
                "iteration_started",
                json!({ "iteration": format!("{iteration}") }),
            )?,
            IterationEvent::Reasoning(StreamPart::Delta(text)) => {
                self.emit_text("reasoning_delta", text)?;
            }
            IterationEvent::Reasoning(StreamPart::End) => {
                self.emit("reasoning_ended", json!({}))?;
            }
            IterationEvent::Output(StreamPart::Delta(text)) => {
                self.emit_text("output_delta", text)?;
            }
            IterationEvent::Output(StreamPart::End) => {
                self.emit("output_ended", json!({}))?;
            }
            IterationEvent::ToolResult(StreamPart::Delta((call, output))) => {
                self.emit("tool_result_started", json!({}))?;
                self.emit_text("tool_call_id_delta", call.id)?;
                self.emit_text("tool_name_delta", call.name)?;
                self.emit_text("tool_arguments_delta", call.arguments_json)?;
                self.emit_text("tool_output_delta", output.content)?;
                self.emit("tool_result_ended", json!({ "ok": output.ok }))?;
            }
            IterationEvent::ToolResult(StreamPart::End) => {
                self.emit("tool_results_ended", json!({}))?;
            }
            IterationEvent::Usage { usage } => {
                self.emit("usage", usage_payload(usage))?;
            }
            IterationEvent::Ended => self.emit("iteration_ended", json!({}))?,
        }
        Ok(())
    }

    fn emit_text(&mut self, event_type: &str, text: String) -> Result<(), AgentWorkflowError> {
        self.emit_with(event_type, |sink| {
            let mut payload = Object::begin(sink);
            payload.str("text", &text);
            payload.end();
        })
    }

    fn emit(&mut self, event_type: &str, payload: Value) -> Result<(), AgentWorkflowError> {
        self.emit_with(event_type, |sink| write_value(sink, &payload))
    }

    /// Emits one Event written straight into shared bulk JSON.
    fn emit_with(
        &mut self,
        event_type: &str,
        payload: impl Fn(&mut dyn Sink),
    ) -> Result<(), AgentWorkflowError> {
        if self.listening {
            let session = format!("{}", self.session);
            let sequence = Value::from(self.sequence);
            let event = JsonText::try_encode_object(|object| {
                payload(object.field("payload"));
                object.value("sequence", &sequence);
                object.str("session", &session);
                object.str("type", event_type);
            });
            match event {
                Ok(event) => self.workflow.emit::<SessionOutputEvent>(event)?,
                Err(_error) => log::warn!(
                    "Agent session {} Event {} does not fit in bulk memory",
                    self.session,
                    self.sequence
                ),
            }
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(AgentWorkflowError::SequenceOverflow)?;
        Ok(())
    }
}

fn usage_payload(usage: ProviderUsage) -> Value {
    let mut payload = Map::new();
    for (name, value) in [
        ("input_tokens", usage.input_tokens),
        ("output_tokens", usage.output_tokens),
        ("cache_read_tokens", usage.cache_read_tokens),
        ("cache_write_tokens", usage.cache_write_tokens),
    ] {
        if let Some(value) = value {
            payload.insert(name.into(), json!(value));
        }
    }
    Value::Object(payload)
}

const fn close_reason(reason: SessionCloseReason) -> &'static str {
    match reason {
        SessionCloseReason::Requested => "requested",
        SessionCloseReason::Deleted => "deleted",
        SessionCloseReason::RuntimeShutdown => "runtime_shutdown",
    }
}

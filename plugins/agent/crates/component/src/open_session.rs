use alloc::{collections::VecDeque, format, rc::Rc, string::String, vec::Vec};

use barracuda_agent_runtime::{
    stream::StreamPart, AgentRuntime, InputRequestId, InputRequestKind, IterationEvent,
    IterationId, OpenSessionError as RuntimeOpenSessionError, RuntimeError, SessionCloseReason,
    SessionEvent, SessionId, ToolCall, ToolOutput, TurnEvent, TurnId, TurnOrigin,
};
use barracuda_event_router::{
    rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, RpcResult, RpcStream, Streaming, Unary,
};
use futures_lite::{stream, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::convert;
use crate::dto::FixedStr;
use crate::session::SessionRegistry;

pub use crate::dto::{OpenSessionRequest, OpenSessionResponseField, OpenSessionResponseFrame};

/// Logical item returned by the `session.open` stream.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenSessionResponse {
    /// Confirms that the control handle and event stream opened.
    Opened {
        /// Opened session.
        session: SessionId,
    },
    /// One event from the opened Agent session.
    Event {
        /// Session that emitted the event.
        session: SessionId,
        /// Transport-stable event representation.
        event: SessionEventDto,
    },
}

/// Failure while rebuilding one logical `session.open` response from chunks.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OpenSessionDecodeError {
    /// Chunks from different sessions or logical response kinds were interleaved.
    #[error("session.open chunks are out of order")]
    InvalidSequence,
    /// The completed chunk sequence was not a valid logical response.
    #[error("session.open response JSON is invalid")]
    InvalidJson,
}

/// Stateful decoder for the typed, chunked `session.open` response stream.
#[derive(Default)]
pub struct OpenSessionResponseDecoder {
    session: Option<crate::dto::SessionIdDto>,
    opened: Option<bool>,
    json: String,
}

impl OpenSessionResponseDecoder {
    /// Creates an empty decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            session: None,
            opened: None,
            json: String::new(),
        }
    }

    /// Absorbs one frame and returns a response when its final chunk arrives.
    ///
    /// # Errors
    ///
    /// Returns an error for interleaved sequences or invalid completed JSON.
    pub fn push(
        &mut self,
        frame: OpenSessionResponseFrame,
    ) -> Result<Option<OpenSessionResponse>, OpenSessionDecodeError> {
        let (opened, complete) = response_field_parts(frame.field);
        if self.session.is_some_and(|session| session != frame.session)
            || self.opened.is_some_and(|current| current != opened)
        {
            self.reset();
            return Err(OpenSessionDecodeError::InvalidSequence);
        }
        self.session = Some(frame.session);
        self.opened = Some(opened);
        self.json.push_str(frame.value.as_str());
        if !complete {
            return Ok(None);
        }

        let response = match serde_json::from_str(&self.json) {
            Ok(response) => response,
            Err(_error) => {
                self.reset();
                return Err(OpenSessionDecodeError::InvalidJson);
            }
        };
        let kind_matches = matches!(
            (&response, opened),
            (OpenSessionResponse::Opened { .. }, true)
        ) || matches!(
            (&response, opened),
            (OpenSessionResponse::Event { .. }, false)
        );
        self.reset();
        if !kind_matches {
            return Err(OpenSessionDecodeError::InvalidSequence);
        }
        Ok(Some(response))
    }

    fn reset(&mut self) {
        self.session = None;
        self.opened = None;
        self.json.clear();
    }
}

/// Transport-stable representation of an Agent `SessionEvent`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEventDto {
    /// A caller-visible turn started.
    TurnStarted {
        /// Session-local turn identifier.
        turn: TurnId,
        /// Cause of the new turn.
        origin: TurnOrigin,
    },
    /// The turn paused for caller input.
    InputRequested {
        /// Request identifier required by `session.respond`.
        request: InputRequestId,
        /// Semantic input required by the Agent.
        kind: InputRequestKind,
    },
    /// A root Agent iteration started.
    IterationStarted {
        /// Iteration identifier.
        iteration: IterationId,
    },
    /// Incremental model reasoning text.
    ReasoningDelta {
        /// Text fragment.
        text: String,
    },
    /// End of reasoning text for the iteration.
    ReasoningEnded,
    /// Incremental assistant-visible text.
    OutputDelta {
        /// Text fragment.
        text: String,
    },
    /// End of assistant-visible model text for the iteration.
    OutputEnded,
    /// One completed tool execution.
    ToolResult {
        /// Original model-requested tool call.
        call: ToolCall,
        /// Tool execution result.
        output: ToolOutputDto,
    },
    /// End of tool results for the iteration.
    ToolResultsEnded,
    /// The root Agent iteration ended.
    IterationEnded,
    /// Provider usage metadata was emitted for the iteration.
    Usage,
    /// Incremental assistant-visible effect output.
    EffectOutputDelta {
        /// Text fragment.
        text: String,
    },
    /// End of assistant-visible effect output.
    EffectOutputEnded,
    /// Recoverable error scoped to the active turn.
    TurnError {
        /// Display-safe error description.
        message: String,
    },
    /// The caller-visible turn ended.
    TurnEnded {
        /// Session-local turn identifier.
        turn: TurnId,
    },
    /// Recoverable error scoped to the session.
    SessionError {
        /// Display-safe error description.
        message: String,
    },
    /// The opened session stream closed normally.
    Closed {
        /// Cause of closure.
        reason: SessionCloseReasonDto,
    },
}

impl From<SessionEvent> for SessionEventDto {
    fn from(event: SessionEvent) -> Self {
        match event {
            SessionEvent::Turn(event) => event.into(),
            SessionEvent::Error(error) => Self::SessionError {
                message: format!("{error}"),
            },
            SessionEvent::Closed(reason) => Self::Closed {
                reason: reason.into(),
            },
        }
    }
}

impl From<TurnEvent> for SessionEventDto {
    fn from(event: TurnEvent) -> Self {
        match event {
            TurnEvent::Started { turn, origin } => Self::TurnStarted { turn, origin },
            TurnEvent::InputRequested { request, kind } => Self::InputRequested { request, kind },
            TurnEvent::Iteration(event) => event.into(),
            TurnEvent::EffectOutput(StreamPart::Delta(text)) => Self::EffectOutputDelta { text },
            TurnEvent::EffectOutput(StreamPart::End) => Self::EffectOutputEnded,
            TurnEvent::Error(error) => Self::TurnError {
                message: format!("{error}"),
            },
            TurnEvent::Ended { turn } => Self::TurnEnded { turn },
        }
    }
}

impl From<IterationEvent> for SessionEventDto {
    #[allow(unreachable_patterns)]
    fn from(event: IterationEvent) -> Self {
        match event {
            IterationEvent::Started { iteration } => Self::IterationStarted { iteration },
            IterationEvent::Reasoning(StreamPart::Delta(text)) => Self::ReasoningDelta { text },
            IterationEvent::Reasoning(StreamPart::End) => Self::ReasoningEnded,
            IterationEvent::Output(StreamPart::Delta(text)) => Self::OutputDelta { text },
            IterationEvent::Output(StreamPart::End) => Self::OutputEnded,
            IterationEvent::ToolResult(StreamPart::Delta((call, output))) => Self::ToolResult {
                call,
                output: output.into(),
            },
            IterationEvent::ToolResult(StreamPart::End) => Self::ToolResultsEnded,
            IterationEvent::Ended => Self::IterationEnded,
            _ => Self::Usage,
        }
    }
}

/// Transport representation of Agent tool output.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ToolOutputDto {
    /// Tool-produced content.
    pub content: String,
    /// Whether the tool reported success.
    pub ok: bool,
}

impl From<ToolOutput> for ToolOutputDto {
    fn from(output: ToolOutput) -> Self {
        Self {
            content: output.content,
            ok: output.ok,
        }
    }
}

/// Transport representation of why an opened session stream closed.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCloseReasonDto {
    /// The caller explicitly closed the session lease.
    Requested,
    /// The session was deleted.
    Deleted,
    /// The owning runtime shut down.
    RuntimeShutdown,
}

impl From<SessionCloseReason> for SessionCloseReasonDto {
    fn from(reason: SessionCloseReason) -> Self {
        match reason {
            SessionCloseReason::Requested => Self::Requested,
            SessionCloseReason::Deleted => Self::Deleted,
            SessionCloseReason::RuntimeShutdown => Self::RuntimeShutdown,
        }
    }
}

/// Failure returned by `session.open`.
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Immutable,
    IntoBytes,
    KnownLayout,
    TryFromBytes,
)]
#[serde(rename_all = "snake_case")]
pub enum OpenSessionError {
    /// The requested session does not exist.
    SessionNotFound,
    /// Another caller already owns the session stream.
    AlreadyOpen,
    /// The Agent runtime worker stopped.
    WorkerStopped,
    /// A session event could not be encoded into one frame.
    InvalidEvent,
}

/// RPC corresponding to `AgentRuntime::open_session`.
pub struct OpenSession;

#[rpc_dynamic]
impl RpcMethod for OpenSession {
    const ADDRESS: &'static str = "session.open";
    type Request = OpenSessionRequest;
    type Response = OpenSessionResponseFrame;
    type Error = OpenSessionError;
    type Input = Unary;
    type Output = Streaming;
}

/// Builds the reusable handler for [`OpenSession`].
pub fn open_session_handler(
    runtime: Rc<AgentRuntime>,
    registry: SessionRegistry,
) -> impl RpcHandler<OpenSession>
where
{
    move |_context, request: RpcFrame<OpenSessionRequest>| {
        let runtime = Rc::clone(&runtime);
        let registry = registry.clone();
        async move {
            let session = convert::session_from_wire(request.view()?.session);
            let (control, events) = match runtime.open_session(session).await {
                Ok(opened) => opened,
                Err(error) => {
                    return Ok(RpcStream::new(stream::once(Ok(Err(map_open_error(error))))));
                }
            };
            registry.insert(session, control);
            let opened = OpenSessionResponse::Opened { session };
            let opened_frames = match frames_from_open_session_response(session, &opened) {
                Ok(frames) => frames,
                Err(_error) => {
                    return Ok(RpcStream::new(stream::once(Ok(Err(
                        OpenSessionError::InvalidEvent,
                    )))));
                }
            };
            let events = open_session_stream(events, registry, session);
            Ok(RpcStream::new(
                stream::iter(opened_frames.into_iter().map(|frame| Ok(Ok(frame)))).chain(events),
            ))
        }
    }
}

fn open_session_stream(
    events: barracuda_agent_runtime::SessionStream,
    registry: SessionRegistry,
    session: SessionId,
) -> impl Stream<Item = RpcResult<Result<OpenSessionResponseFrame, OpenSessionError>>> {
    stream::unfold(
        (events, registry, session, false, VecDeque::new()),
        |(mut events, registry, session, terminal, mut pending)| async move {
            if let Some(frame) = pending.pop_front() {
                return Some((
                    Ok(Ok(frame)),
                    (events, registry, session, terminal, pending),
                ));
            }
            if terminal {
                return None;
            }
            match events.next().await {
                Some(Ok(event)) => {
                    let terminal = matches!(&event, SessionEvent::Closed(_));
                    if terminal {
                        registry.remove(session);
                    }
                    let response = OpenSessionResponse::Event {
                        session,
                        event: event.into(),
                    };
                    match frames_from_open_session_response(session, &response) {
                        Ok(frames) => {
                            pending.extend(frames);
                            pending.pop_front().map(|frame| {
                                (
                                    Ok(Ok(frame)),
                                    (events, registry, session, terminal, pending),
                                )
                            })
                        }
                        Err(_error) => Some((
                            Ok(Err(OpenSessionError::InvalidEvent)),
                            (events, registry, session, terminal, pending),
                        )),
                    }
                }
                Some(Err(_error)) => {
                    registry.remove(session);
                    Some((
                        Ok(Err(OpenSessionError::WorkerStopped)),
                        (events, registry, session, true, pending),
                    ))
                }
                None => None,
            }
        },
    )
}

/// Encodes one logical response into fixed-size typed chunks.
///
/// # Errors
///
/// Returns [`OpenSessionError::InvalidEvent`] if JSON serialization fails.
pub fn frames_from_open_session_response(
    session: SessionId,
    response: &OpenSessionResponse,
) -> Result<Vec<OpenSessionResponseFrame>, OpenSessionError> {
    let json = serde_json::to_string(response).map_err(|_error| OpenSessionError::InvalidEvent)?;
    let opened = matches!(response, OpenSessionResponse::Opened { .. });
    let chunks = utf8_chunks(&json, FixedStr::<506>::capacity());
    let last = chunks.len().saturating_sub(1);
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let complete = index == last;
            Ok(OpenSessionResponseFrame {
                session: convert::session_to_wire(session),
                value: FixedStr::new(chunk).map_err(|_error| OpenSessionError::InvalidEvent)?,
                field: response_field(opened, complete),
                reserved: 0,
            })
        })
        .collect()
}

const fn response_field(opened: bool, complete: bool) -> OpenSessionResponseField {
    match (opened, complete) {
        (true, false) => OpenSessionResponseField::OpenedMore,
        (true, true) => OpenSessionResponseField::OpenedComplete,
        (false, false) => OpenSessionResponseField::EventMore,
        (false, true) => OpenSessionResponseField::EventComplete,
    }
}

const fn response_field_parts(field: OpenSessionResponseField) -> (bool, bool) {
    match field {
        OpenSessionResponseField::OpenedMore => (true, false),
        OpenSessionResponseField::OpenedComplete => (true, true),
        OpenSessionResponseField::EventMore => (false, false),
        OpenSessionResponseField::EventComplete => (false, true),
    }
}

fn utf8_chunks(value: &str, capacity: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < value.len() {
        let mut end = core::cmp::min(start.saturating_add(capacity), value.len());
        while !value.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        if end == start {
            break;
        }
        if let Some(chunk) = value.get(start..end) {
            chunks.push(chunk);
        }
        start = end;
    }
    chunks
}

fn map_open_error(error: RuntimeError) -> OpenSessionError {
    match error {
        RuntimeError::OpenSession(RuntimeOpenSessionError::SessionNotFound(_)) => {
            OpenSessionError::SessionNotFound
        }
        RuntimeError::OpenSession(RuntimeOpenSessionError::AlreadyOpen(_)) => {
            OpenSessionError::AlreadyOpen
        }
        RuntimeError::OpenSession(RuntimeOpenSessionError::WorkerStopped) => {
            OpenSessionError::WorkerStopped
        }
        _ => OpenSessionError::WorkerStopped,
    }
}

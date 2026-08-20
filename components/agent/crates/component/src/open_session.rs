use alloc::{collections::VecDeque, format, rc::Rc, string::String, vec::Vec};

use barracuda_agent_runtime::{
    stream::StreamPart, AgentRuntime, InputRequestId, InputRequestKind, IterationEvent,
    IterationId, OpenSessionError as RuntimeOpenSessionError, RuntimeError, SessionCloseReason,
    SessionEvent, SessionId, ToolCall, ToolOutput, TurnEvent, TurnId, TurnOrigin,
};
use barracuda_event_router::{
    RpcError, RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary,
};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};
use futures_lite::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::{
    dto::SessionIdDto, session::SessionRegistry, wire::read_payload, wire::write_payload,
    wire::AgentWireError,
};

const FRAME_PAYLOAD_SIZE: usize = 125;

/// Request corresponding to `AgentRuntime::open_session`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct OpenSessionRequest {
    session: SessionIdDto,
}

impl OpenSessionRequest {
    /// Creates an open request for `session`.
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        Self {
            session: session.into(),
        }
    }

    pub(crate) fn session(self) -> SessionId {
        self.session.into()
    }
}

/// Logical item returned by the `agent.open_session` stream.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenSessionResponse {
    /// Confirms that the control handle and event stream were opened.
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

impl OpenSessionResponse {
    pub(crate) fn from_event(session: SessionId, event: SessionEvent) -> Self {
        Self::Event {
            session,
            event: event.into(),
        }
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

#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
enum FrameEnd {
    More,
    End,
}

/// One frame of a variable-length [`OpenSessionResponse`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct OpenSessionResponseFrame {
    length: u16,
    end: FrameEnd,
    payload: [u8; FRAME_PAYLOAD_SIZE],
}

impl OpenSessionResponseFrame {
    fn new(bytes: &[u8], end: FrameEnd) -> Result<Self, AgentWireError> {
        let mut payload = [0; FRAME_PAYLOAD_SIZE];
        let length = write_payload(&mut payload, bytes)?;
        Ok(Self {
            length,
            end,
            payload,
        })
    }

    fn bytes(&self) -> Result<&[u8], AgentWireError> {
        read_payload(&self.payload, self.length)
    }

    /// Returns whether this frame ends one logical event.
    #[must_use]
    pub fn is_end(&self) -> bool {
        self.end == FrameEnd::End
    }
}

/// Failure returned by `agent.open_session`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum OpenSessionError {
    /// The requested session does not exist.
    SessionNotFound,
    /// Another caller already owns the session stream.
    AlreadyOpen,
    /// The Agent runtime worker stopped.
    WorkerStopped,
    /// A session event could not be encoded.
    InvalidEvent,
}

/// RPC corresponding to `AgentRuntime::open_session`.
pub struct OpenSession;

impl RpcMethod for OpenSession {
    const ADDRESS: &'static str = "agent.open_session";
    type Request = OpenSessionRequest;
    type Response = OpenSessionResponseFrame;
    type Error = OpenSessionError;
    type Input = Unary;
    type Output = Streaming;
}

/// Builds the reusable handler for [`OpenSession`].
pub fn open_session_handler<Filesystem, Http>(
    runtime: Rc<AgentRuntime<Filesystem, Http>>,
    registry: SessionRegistry,
) -> impl RpcHandler<OpenSession>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    move |_context, request: RpcFrame<OpenSessionRequest>| {
        let runtime = Rc::clone(&runtime);
        let registry = registry.clone();
        async move {
            let session = request.view()?.session();
            let (control, events) = match runtime.open_session(session).await {
                Ok(opened) => opened,
                Err(error) => {
                    return Ok(RpcStream::new(stream::once(Ok(Err(map_open_error(error))))))
                }
            };
            registry.insert(session, control);
            let opened = OpenSessionResponse::Opened { session };
            let pending = frames_from_open_session_response(&opened)
                .map_err(|_error| RpcError::InvalidFrameState)?
                .into();
            Ok(open_session_stream(OpenStreamState {
                session,
                events,
                registry,
                pending,
                finish_after_pending: false,
            }))
        }
    }
}

struct OpenStreamState {
    session: SessionId,
    events: barracuda_agent_runtime::SessionStream,
    registry: SessionRegistry,
    pending: VecDeque<OpenSessionResponseFrame>,
    finish_after_pending: bool,
}

fn open_session_stream(
    state: OpenStreamState,
) -> RpcStream<Result<OpenSessionResponseFrame, OpenSessionError>> {
    RpcStream::new(stream::unfold(state, |mut state| async move {
        if let Some(frame) = state.pending.pop_front() {
            return Some((Ok(Ok(frame)), state));
        }
        if state.finish_after_pending {
            return None;
        }
        match state.events.next().await {
            Some(Ok(event)) => {
                let terminal = matches!(&event, SessionEvent::Closed(_));
                let response = OpenSessionResponse::from_event(state.session, event);
                match frames_from_open_session_response(&response) {
                    Ok(frames) => {
                        state.pending = frames.into();
                        state.finish_after_pending = terminal;
                        if terminal {
                            state.registry.remove(state.session);
                        }
                        state
                            .pending
                            .pop_front()
                            .map(|frame| (Ok(Ok(frame)), state))
                    }
                    Err(_error) => {
                        state.registry.remove(state.session);
                        state.finish_after_pending = true;
                        Some((Ok(Err(OpenSessionError::InvalidEvent)), state))
                    }
                }
            }
            Some(Err(_error)) => {
                state.registry.remove(state.session);
                state.finish_after_pending = true;
                Some((Ok(Err(OpenSessionError::WorkerStopped)), state))
            }
            None => {
                state.registry.remove(state.session);
                None
            }
        }
    }))
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

/// Encodes one logical open-session response into frames.
pub fn frames_from_open_session_response(
    response: &OpenSessionResponse,
) -> Result<Vec<OpenSessionResponseFrame>, AgentWireError> {
    let bytes = serde_json::to_vec(response)?;
    let chunk_count = bytes.chunks(FRAME_PAYLOAD_SIZE).count();
    bytes
        .chunks(FRAME_PAYLOAD_SIZE)
        .enumerate()
        .map(|(index, chunk)| {
            let end = if index.saturating_add(1) == chunk_count {
                FrameEnd::End
            } else {
                FrameEnd::More
            };
            OpenSessionResponseFrame::new(chunk, end)
        })
        .collect()
}

/// Decodes one logical open-session response from its complete frame sequence.
pub fn open_session_response_from_frames(
    frames: impl IntoIterator<Item = OpenSessionResponseFrame>,
) -> Result<OpenSessionResponse, AgentWireError> {
    let mut bytes = Vec::new();
    let mut ended = false;
    for frame in frames {
        if ended {
            return Err(AgentWireError::InvalidFrame);
        }
        bytes.extend_from_slice(frame.bytes()?);
        ended = frame.is_end();
    }
    if !ended {
        return Err(AgentWireError::InvalidFrame);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

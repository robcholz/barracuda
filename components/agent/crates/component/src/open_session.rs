use alloc::{format, rc::Rc, string::String};

use barracuda_agent_runtime::{
    stream::StreamPart, AgentRuntime, InputRequestId, InputRequestKind, IterationEvent,
    IterationId, OpenSessionError as RuntimeOpenSessionError, RuntimeError, SessionCloseReason,
    SessionEvent, SessionId, ToolCall, ToolOutput, TurnEvent, TurnId, TurnOrigin,
};
use barracuda_event_router::{
    rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, RpcResult, RpcStream, Streaming, Unary,
};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};
use futures_lite::{stream, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::convert;
use crate::dto::FixedStr;
use crate::session::SessionRegistry;

pub use crate::dto::{OpenSessionRequest, OpenSessionResponseFrame};

/// Logical item returned by the `agent.open_session` stream.
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

/// Failure returned by `agent.open_session`.
#[repr(u8)]
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Immutable, IntoBytes, KnownLayout,
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
            let session = convert::session_from_wire(request.view()?.session);
            let (control, events) = match runtime.open_session(session).await {
                Ok(opened) => opened,
                Err(error) => {
                    return Ok(RpcStream::new(stream::once(Ok(Err(map_open_error(error))))))
                }
            };
            registry.insert(session, control);
            let opened = OpenSessionResponse::Opened { session };
            let opened_frame = match encode_response(session, &opened) {
                Ok(frame) => frame,
                Err(_error) => {
                    return Ok(RpcStream::new(stream::once(Ok(Err(
                        OpenSessionError::InvalidEvent,
                    )))))
                }
            };
            let events = open_session_stream(events, registry, session);
            Ok(RpcStream::new(stream::iter([Ok(Ok(opened_frame))]).chain(events)))
        }
    }
}

fn open_session_stream(
    events: barracuda_agent_runtime::SessionStream,
    registry: SessionRegistry,
    session: SessionId,
) -> impl Stream<Item = RpcResult<Result<OpenSessionResponseFrame, OpenSessionError>>> {
    stream::unfold(
        (events, registry, session, false),
        |(mut events, registry, session, terminal)| async move {
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
                    match encode_response(session, &response) {
                        Ok(frame) => Some((Ok(Ok(frame)), (events, registry, session, terminal))),
                        Err(_error) => Some((
                            Ok(Err(OpenSessionError::InvalidEvent)),
                            (events, registry, session, terminal),
                        )),
                    }
                }
                Some(Err(_error)) => {
                    registry.remove(session);
                    Some((
                        Ok(Err(OpenSessionError::WorkerStopped)),
                        (events, registry, session, true),
                    ))
                }
                None => None,
            }
        },
    )
}

fn encode_response(
    session: SessionId,
    response: &OpenSessionResponse,
) -> Result<OpenSessionResponseFrame, OpenSessionError> {
    let json = serde_json::to_string(response).map_err(|_error| OpenSessionError::InvalidEvent)?;
    let json = FixedStr::new(&json).map_err(|_error| OpenSessionError::InvalidEvent)?;
    Ok(OpenSessionResponseFrame {
        session: convert::session_to_wire(session),
        json,
    })
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

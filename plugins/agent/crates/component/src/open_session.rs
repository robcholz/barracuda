use alloc::{rc::Rc, string::String};
use core::fmt::Write as _;

use barracuda_agent_runtime::{
    stream::StreamPart, AgentRuntime, InputRequestId, InputRequestKind, IterationEvent,
    IterationId, OpenSessionError as RuntimeOpenSessionError, ProviderUsage, RuntimeError,
    SessionCloseReason, SessionEvent, ToolCall, ToolOutput, TurnEvent, TurnId, TurnOrigin,
};
use barracuda_event_router::{
    json_schema, JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
};
use serde::Deserialize;

use crate::json::{parse_session, AgentRpcError, ErrorResponse, OpenedResponse};
use crate::session::SessionRegistry;

/// Opens a session control lease and subscribes it to `session.event`.
pub struct OpenSession;

impl JsonRpcSchema for OpenSession {
    const ADDRESS: &'static str = "session.open";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("open", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("open", response);
    const MAX_REQUEST_BYTES: usize = 48;
    const MAX_RESPONSE_BYTES: usize = 64;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenSessionRequest<'a> {
    #[serde(borrow)]
    session: &'a str,
}

/// Builds the unary JSON handler for [`OpenSession`].
pub fn open_session_handler(
    runtime: Rc<AgentRuntime>,
    registry: SessionRegistry,
) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let runtime = Rc::clone(&runtime);
        let registry = registry.clone();
        async move {
            let request = request.deserialize::<OpenSessionRequest<'_>>()?;
            let session = match parse_session(request.session) {
                Ok(session) => session,
                Err(error) => return response.write(&ErrorResponse(error)).await,
            };
            match runtime.open_session(session).await {
                Ok((control, events)) => {
                    let run = registry.insert(session, control, events);
                    response.write(&OpenedResponse { session, run }).await
                }
                Err(error) => response.write(&ErrorResponse(map_open_error(error))).await,
            }
        }
    }
}

fn map_open_error(error: RuntimeError) -> AgentRpcError {
    match error {
        RuntimeError::OpenSession(RuntimeOpenSessionError::SessionNotFound(_)) => {
            AgentRpcError::SessionNotFound
        }
        RuntimeError::OpenSession(RuntimeOpenSessionError::AlreadyOpen(_)) => {
            AgentRpcError::AlreadyOpen
        }
        RuntimeError::OpenSession(RuntimeOpenSessionError::WorkerStopped) => {
            AgentRpcError::WorkerStopped
        }
        _ => AgentRpcError::WorkerStopped,
    }
}

/// Owned runtime event awaiting bounded semantic Event emission.
pub(crate) enum SessionEventDocument {
    TurnStarted {
        turn: TurnId,
        origin: TurnOrigin,
    },
    InputRequested {
        request: InputRequestId,
        kind: InputRequestKind,
    },
    IterationStarted {
        iteration: IterationId,
    },
    ReasoningDelta {
        text: String,
    },
    ReasoningEnded,
    OutputDelta {
        text: String,
    },
    OutputEnded,
    ToolResult {
        call: ToolCall,
        output: ToolOutput,
    },
    ToolResultsEnded,
    IterationEnded,
    Usage {
        usage: ProviderUsage,
    },
    EffectOutputDelta {
        text: String,
    },
    EffectOutputEnded,
    TurnError {
        message: ErrorText,
    },
    TurnEnded {
        turn: TurnId,
    },
    SessionError {
        message: ErrorText,
    },
    Closed {
        reason: CloseReasonDocument,
    },
    StreamError {
        error: &'static str,
    },
}

impl From<SessionEvent> for SessionEventDocument {
    fn from(event: SessionEvent) -> Self {
        match event {
            SessionEvent::Turn(event) => event.into(),
            SessionEvent::Error(error) => Self::SessionError {
                message: ErrorText::from_display(error),
            },
            SessionEvent::Closed(reason) => Self::Closed {
                reason: reason.into(),
            },
        }
    }
}

impl From<TurnEvent> for SessionEventDocument {
    fn from(event: TurnEvent) -> Self {
        match event {
            TurnEvent::Started { turn, origin } => Self::TurnStarted { turn, origin },
            TurnEvent::InputRequested { request, kind } => Self::InputRequested { request, kind },
            TurnEvent::Iteration(event) => event.into(),
            TurnEvent::EffectOutput(StreamPart::Delta(text)) => Self::EffectOutputDelta { text },
            TurnEvent::EffectOutput(StreamPart::End) => Self::EffectOutputEnded,
            TurnEvent::Error(error) => Self::TurnError {
                message: ErrorText::from_display(error),
            },
            TurnEvent::Ended { turn } => Self::TurnEnded { turn },
        }
    }
}

impl From<IterationEvent> for SessionEventDocument {
    fn from(event: IterationEvent) -> Self {
        match event {
            IterationEvent::Started { iteration } => Self::IterationStarted { iteration },
            IterationEvent::Reasoning(StreamPart::Delta(text)) => Self::ReasoningDelta { text },
            IterationEvent::Reasoning(StreamPart::End) => Self::ReasoningEnded,
            IterationEvent::Output(StreamPart::Delta(text)) => Self::OutputDelta { text },
            IterationEvent::Output(StreamPart::End) => Self::OutputEnded,
            IterationEvent::ToolResult(StreamPart::Delta((call, output))) => {
                Self::ToolResult { call, output }
            }
            IterationEvent::ToolResult(StreamPart::End) => Self::ToolResultsEnded,
            IterationEvent::Usage { usage } => Self::Usage { usage },
            IterationEvent::Ended => Self::IterationEnded,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CloseReasonDocument {
    Requested,
    Deleted,
    RuntimeShutdown,
}

impl CloseReasonDocument {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Deleted => "deleted",
            Self::RuntimeShutdown => "runtime_shutdown",
        }
    }
}

impl From<SessionCloseReason> for CloseReasonDocument {
    fn from(reason: SessionCloseReason) -> Self {
        match reason {
            SessionCloseReason::Requested => Self::Requested,
            SessionCloseReason::Deleted => Self::Deleted,
            SessionCloseReason::RuntimeShutdown => Self::RuntimeShutdown,
        }
    }
}

const ERROR_TEXT_CAPACITY: usize = 192;

pub(crate) struct ErrorText {
    bytes: [u8; ERROR_TEXT_CAPACITY],
    len: usize,
    truncated: bool,
}

impl ErrorText {
    fn from_display(value: impl core::fmt::Display) -> Self {
        let mut text = Self {
            bytes: [0; ERROR_TEXT_CAPACITY],
            len: 0,
            truncated: false,
        };
        let _ignored = write!(text, "{value}");
        text
    }

    pub(crate) fn as_str(&self) -> &str {
        core::str::from_utf8(self.bytes.get(..self.len).unwrap_or(&[])).unwrap_or("")
    }

    pub(crate) const fn truncated(&self) -> bool {
        self.truncated
    }
}

impl core::fmt::Write for ErrorText {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        let available = ERROR_TEXT_CAPACITY.saturating_sub(self.len);
        let mut take = core::cmp::min(available, value.len());
        while !value.is_char_boundary(take) {
            take = take.saturating_sub(1);
        }
        let Some(source) = value.as_bytes().get(..take) else {
            return Ok(());
        };
        let Some(destination) = self.bytes.get_mut(self.len..self.len.saturating_add(take)) else {
            self.truncated = true;
            return Ok(());
        };
        destination.copy_from_slice(source);
        self.len = self.len.saturating_add(take);
        if take < value.len() {
            self.truncated = true;
        }
        Ok(())
    }
}

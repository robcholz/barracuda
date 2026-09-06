use core::fmt;

use barracuda_agent_runtime::{InputRequestId, SessionControlError, SessionId};
use barracuda_event_router::{JsonPayload, RpcError};

/// Stable business failures returned by the Agent JSON RPCs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentRpcError {
    /// A semantically invalid identifier, value, or bounded field was supplied.
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

impl AgentRpcError {
    /// Returns the stable response code.
    #[must_use]
    pub const fn code(self) -> &'static str {
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

impl core::fmt::Display for AgentRpcError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.code())
    }
}

/// Allocation-free JSON business error response.
pub struct ErrorResponse(pub AgentRpcError);

impl JsonPayload for ErrorResponse {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        12_usize
            .checked_add(self.0.code().len())
            .ok_or(RpcError::InvalidFrameState)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_payload(destination, |writer| {
            write!(writer, "{{\"error\":\"{}\"}}", self.0.code())
        })
    }
}

/// Allocation-free business error response retaining a parsed session.
pub struct SessionErrorResponse {
    /// Session whose operation failed.
    pub session: SessionId,
    /// Stable business failure.
    pub error: AgentRpcError,
}

impl JsonPayload for SessionErrorResponse {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        measure(|writer| {
            write!(
                writer,
                "{{\"session\":\"{}\",\"error\":\"{}\"}}",
                self.session,
                self.error.code()
            )
        })
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_payload(destination, |writer| {
            write!(
                writer,
                "{{\"session\":\"{}\",\"error\":\"{}\"}}",
                self.session,
                self.error.code()
            )
        })
    }
}

/// Allocation-free JSON response carrying one session identifier.
pub struct SessionResponse {
    /// Session identifier to expose.
    pub session: SessionId,
}

impl JsonPayload for SessionResponse {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        measure(|writer| write!(writer, "{{\"session\":\"{}\"}}", self.session))
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_payload(destination, |writer| {
            write!(writer, "{{\"session\":\"{}\"}}", self.session)
        })
    }
}

/// Allocation-free response confirming one event subscription.
pub struct OpenedResponse {
    /// Opened session.
    pub session: SessionId,
    /// Component-local subscription run number.
    pub run: u32,
}

impl JsonPayload for OpenedResponse {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        measure(|writer| {
            write!(
                writer,
                "{{\"session\":\"{}\",\"run\":\"run-{}\"}}",
                self.session, self.run
            )
        })
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_payload(destination, |writer| {
            write!(
                writer,
                "{{\"session\":\"{}\",\"run\":\"run-{}\"}}",
                self.session, self.run
            )
        })
    }
}

/// Allocation-free bounded session-list page.
pub struct SessionPage<'a> {
    /// Sessions in this page.
    pub sessions: &'a [SessionId],
    /// Offset for the next page when more sessions remain.
    pub next_offset: Option<usize>,
}

impl JsonPayload for SessionPage<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        measure(|writer| write_session_page(writer, self))
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_payload(destination, |writer| write_session_page(writer, self))
    }
}

fn write_session_page(writer: &mut dyn fmt::Write, page: &SessionPage<'_>) -> fmt::Result {
    writer.write_str("{\"sessions\":[")?;
    for (index, session) in page.sessions.iter().enumerate() {
        if index != 0 {
            writer.write_char(',')?;
        }
        write!(writer, "\"{session}\"")?;
    }
    writer.write_str("],\"next_offset\":")?;
    match page.next_offset {
        Some(offset) => write!(writer, "{offset}")?,
        None => writer.write_str("null")?,
    }
    writer.write_char('}')
}

/// Parses a prefixed session ID without allocating on the success path.
pub fn parse_session(value: &str) -> Result<SessionId, AgentRpcError> {
    parse_prefixed(value, "session-").map(SessionId::new)
}

/// Parses a prefixed input-request ID without allocating on the success path.
pub fn parse_input_request(value: &str) -> Result<InputRequestId, AgentRpcError> {
    parse_prefixed(value, "input-").map(InputRequestId::new)
}

fn parse_prefixed(value: &str, prefix: &str) -> Result<u32, AgentRpcError> {
    value
        .strip_prefix(prefix)
        .filter(|digits| !digits.is_empty())
        .and_then(|digits| digits.parse::<u32>().ok())
        .ok_or(AgentRpcError::InvalidRequest)
}

/// Maps the runtime's open-session control failures onto stable JSON codes.
pub fn map_control_error(error: SessionControlError) -> AgentRpcError {
    match error {
        SessionControlError::SessionClosed(_) => AgentRpcError::SessionClosed,
        SessionControlError::NotAwaitingInput(_) => AgentRpcError::NotAwaitingInput,
        SessionControlError::InputRequestMismatch { .. } => AgentRpcError::InputRequestMismatch,
        SessionControlError::WorkerStopped => AgentRpcError::WorkerStopped,
    }
}

fn measure(write: impl FnOnce(&mut dyn fmt::Write) -> fmt::Result) -> Result<usize, RpcError> {
    let mut writer = CountingWriter(0);
    write(&mut writer).map_err(|_error| RpcError::InvalidFrameState)?;
    Ok(writer.0)
}

fn write_payload(
    destination: &mut [u8],
    write: impl FnOnce(&mut dyn fmt::Write) -> fmt::Result,
) -> Result<usize, RpcError> {
    let capacity = destination.len();
    let mut writer = SliceWriter {
        destination,
        written: 0,
    };
    write(&mut writer).map_err(|_error| RpcError::FrameTooLarge {
        size: capacity.saturating_add(1),
        capacity,
    })?;
    Ok(writer.written)
}

struct CountingWriter(usize);

impl fmt::Write for CountingWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.0 = self.0.checked_add(value.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}

struct SliceWriter<'a> {
    destination: &'a mut [u8],
    written: usize,
}

impl fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.written.checked_add(value.len()).ok_or(fmt::Error)?;
        let output = self
            .destination
            .get_mut(self.written..end)
            .ok_or(fmt::Error)?;
        output.copy_from_slice(value.as_bytes());
        self.written = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use barracuda_agent_runtime::SessionId;
    use barracuda_event_router::JsonPayload;

    use super::{AgentRpcError, SessionErrorResponse};

    #[test]
    fn session_error_response_preserves_correlation() {
        let response = SessionErrorResponse {
            session: SessionId::new(9),
            error: AgentRpcError::WorkerStopped,
        };
        let mut output = [0_u8; 64];
        let length = response.write_json(&mut output).expect("write response");

        assert_eq!(
            core::str::from_utf8(output.get(..length).expect("response bytes"))
                .expect("response UTF-8"),
            r#"{"session":"session-9","error":"worker_stopped"}"#
        );
    }
}

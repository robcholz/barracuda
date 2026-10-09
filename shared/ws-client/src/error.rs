//! Errors of the WebSocket client.

use core::fmt;

use embedded_io_async::ErrorKind;

/// Why the opening handshake failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HandshakeError {
    /// The server answered with this HTTP status instead of 101.
    Status(u16),
    /// The response was not an HTTP/1.1 response.
    Malformed,
    /// The response headers did not fit the handshake buffer.
    HeadersTooLarge,
    /// `Upgrade: websocket` or `Connection: Upgrade` was missing.
    NotUpgraded,
    /// `Sec-WebSocket-Accept` was missing or did not match the key.
    BadAccept,
    /// The server selected an extension or subprotocol the client never offered.
    UnexpectedExtension,
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(status) => write!(formatter, "the server answered HTTP {status}"),
            Self::Malformed => formatter.write_str("the server's answer is not HTTP/1.1"),
            Self::HeadersTooLarge => formatter.write_str("the server's headers are too large"),
            Self::NotUpgraded => formatter.write_str("the server did not upgrade to WebSocket"),
            Self::BadAccept => formatter.write_str("the server's Sec-WebSocket-Accept is wrong"),
            Self::UnexpectedExtension => {
                formatter.write_str("the server selected an extension that was not offered")
            }
        }
    }
}

/// Failure of a WebSocket connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WsError {
    /// The URL is not a `ws://` or `wss://` URL with a host.
    InvalidUrl,
    /// The opening handshake failed.
    Handshake(HandshakeError),
    /// Reading or writing the underlying stream failed.
    Io(ErrorKind),
    /// The stream ended without a close frame.
    Eof,
    /// The peer broke the framing rules; the reason names the rule.
    Protocol(&'static str),
    /// A frame or message is larger than the configured limit.
    TooLarge,
    /// A text message or close reason is not valid UTF-8.
    InvalidUtf8,
    /// A control frame payload is longer than 125 bytes.
    ControlTooLong,
}

impl WsError {
    /// The close code to send before dropping the connection, when the
    /// failure is the peer's.
    #[must_use]
    pub const fn close_code(&self) -> Option<u16> {
        match self {
            Self::Protocol(_) => Some(crate::CLOSE_PROTOCOL_ERROR),
            Self::TooLarge => Some(crate::CLOSE_TOO_BIG),
            Self::InvalidUtf8 => Some(1007),
            _ => None,
        }
    }

    pub(crate) fn io<E: embedded_io_async::Error>(error: &E) -> Self {
        Self::Io(error.kind())
    }
}

impl fmt::Display for WsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => formatter.write_str("unsupported WebSocket URL"),
            Self::Handshake(error) => write!(formatter, "WebSocket handshake failed: {error}"),
            Self::Io(kind) => write!(formatter, "connection failed: {kind:?}"),
            Self::Eof => formatter.write_str("the connection ended without a close frame"),
            Self::Protocol(rule) => write!(formatter, "WebSocket protocol error: {rule}"),
            Self::TooLarge => formatter.write_str("WebSocket message is too large"),
            Self::InvalidUtf8 => formatter.write_str("WebSocket text is not UTF-8"),
            Self::ControlTooLong => formatter.write_str("control frame payload over 125 bytes"),
        }
    }
}

impl core::error::Error for WsError {}

impl From<HandshakeError> for WsError {
    fn from(error: HandshakeError) -> Self {
        Self::Handshake(error)
    }
}

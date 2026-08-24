//! Fixed-layout RPC messages for the Agent Component.
//!
//! These types are the transport contract between the Agent Component and its
//! callers. Fixed-layout unary DTOs are `serde`- and `zerocopy`-friendly so a
//! `#[rpc_dynamic]` method can transcode JSON and expose per-field wire access.
//! Text is always carried as a fixed-capacity, NUL-terminated UTF-8 buffer that
//! serializes as a JSON string, never as a byte array. Unbounded text streams
//! across fixed-layout frames whose text chunks are the same C-string type.

use alloc::format;
use alloc::string::String;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

/// A fixed-capacity, UTF-8, NUL-terminated string.
///
/// The buffer holds at most `N - 1` bytes of UTF-8 followed by at least one
/// NUL terminator. It serializes as a JSON string.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Immutable, IntoBytes, KnownLayout, TryFromBytes)]
pub struct FixedStr<const N: usize>([u8; N]);

/// Failure while building a [`FixedStr`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixedStrError {
    /// The text does not fit in the fixed capacity.
    TooLong,
    /// The text contains an embedded NUL byte.
    EmbeddedNul,
}

impl core::fmt::Display for FixedStrError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooLong => formatter.write_str("string exceeds fixed capacity"),
            Self::EmbeddedNul => formatter.write_str("string contains a NUL byte"),
        }
    }
}

impl core::error::Error for FixedStrError {}

impl<const N: usize> FixedStr<N> {
    /// The maximum number of UTF-8 bytes this buffer can hold.
    #[must_use]
    pub const fn capacity() -> usize {
        N.saturating_sub(1)
    }

    /// Builds a fixed string from `value`.
    ///
    /// # Errors
    ///
    /// Returns [`FixedStrError::TooLong`] when `value` does not leave room for
    /// the NUL terminator, or [`FixedStrError::EmbeddedNul`] when `value`
    /// contains a NUL byte.
    pub fn new(value: &str) -> Result<Self, FixedStrError> {
        let bytes = value.as_bytes();
        if bytes.len() >= N {
            return Err(FixedStrError::TooLong);
        }
        if bytes.contains(&0) {
            return Err(FixedStrError::EmbeddedNul);
        }
        let mut buffer = [0u8; N];
        let destination = buffer
            .get_mut(..bytes.len())
            .ok_or(FixedStrError::TooLong)?;
        destination.copy_from_slice(bytes);
        Ok(Self(buffer))
    }

    /// Returns the UTF-8 text up to the NUL terminator.
    #[must_use]
    pub fn as_str(&self) -> &str {
        let end = self.0.iter().position(|&byte| byte == 0);
        core::str::from_utf8(self.0.get(..end.unwrap_or(N)).unwrap_or(&[])).unwrap_or("")
    }
}

impl<const N: usize> Serialize for FixedStr<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de, const N: usize> Deserialize<'de> for FixedStr<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

/// Wire representation of an Agent session identifier.
///
/// Serializes as the prefixed string form used by the Agent runtime, for
/// example `"session-7"`.
#[repr(transparent)]
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Immutable, IntoBytes, KnownLayout, TryFromBytes,
)]
pub struct SessionIdDto(u32);

impl SessionIdDto {
    /// Builds a session identifier from its raw numeric value.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the raw numeric session identifier.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Serialize for SessionIdDto {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("session-{}", self.0))
    }
}

impl<'de> Deserialize<'de> for SessionIdDto {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let digits = value
            .strip_prefix("session-")
            .ok_or_else(|| serde::de::Error::custom("session id must start with \"session-\""))?;
        let id = digits
            .parse::<u32>()
            .map_err(|_| serde::de::Error::custom("invalid session id"))?;
        Ok(Self::new(id))
    }
}

/// Wire representation of an Agent input request identifier.
#[repr(transparent)]
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Immutable, IntoBytes, KnownLayout, TryFromBytes,
)]
pub struct InputRequestIdDto(u32);

impl InputRequestIdDto {
    /// Builds an input request identifier from its raw numeric value.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the raw numeric input request identifier.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Serialize for InputRequestIdDto {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("input-{}", self.0))
    }
}

impl<'de> Deserialize<'de> for InputRequestIdDto {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let digits = value.strip_prefix("input-").ok_or_else(|| {
            serde::de::Error::custom("input request id must start with \"input-\"")
        })?;
        let id = digits
            .parse::<u32>()
            .map_err(|_| serde::de::Error::custom("invalid input request id"))?;
        Ok(Self::new(id))
    }
}

/// Wire representation of Agent session persistence.
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
pub enum SessionPersistenceDto {
    /// Preserve the session across runtime restarts.
    Persistent,
    /// Keep the session only for the current process.
    Ephemeral,
}

/// Wire representation of Agent permission level.
#[repr(u32)]
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
pub enum PermissionLevelDto {
    /// Deny actions with side effects.
    Deny,
    /// Ask before actions with side effects.
    Ask,
    /// Allow every action.
    AllowAll,
}

/// Wire representation of Agent reasoning effort.
#[repr(u32)]
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
pub enum ReasoningEffortDto {
    /// Shortest sound reasoning path.
    Low,
    /// Default balanced reasoning.
    Medium,
    /// Deliberate decomposition and verification.
    High,
    /// Maximum multi-agent reasoning effort.
    Ultra,
}

/// Request corresponding to `session.new`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewSessionRequest {
    /// Persistence policy for the new session.
    pub persistence: SessionPersistenceDto,
}

/// Response corresponding to `session.new`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewSessionResponse {
    /// Identifier of the created session.
    pub session: SessionIdDto,
}

/// Failure returned by `session.new`.
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
pub enum NewSessionError {
    /// The Agent runtime worker stopped.
    WorkerStopped,
    /// Persistent session state could not be initialized.
    Persistence,
}

/// Request corresponding to `session.delete`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeleteSessionRequest {
    /// Identifier of the session to delete.
    pub session: SessionIdDto,
}

/// Failure returned by `session.delete`.
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
pub enum DeleteSessionError {
    /// The requested session does not exist.
    SessionNotFound,
    /// Deletion of this session is already in progress.
    AlreadyDeleting,
    /// The Agent runtime worker stopped.
    WorkerStopped,
    /// Persistent state could not be deleted.
    Storage,
}

/// Request corresponding to `session.cancel`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CancelRequest {
    /// Session to cancel.
    pub session: SessionIdDto,
}

/// Request corresponding to `session.close`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseRequest {
    /// Session to close.
    pub session: SessionIdDto,
}

/// Request corresponding to `session.interrupt`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterruptRequest {
    /// Session to interrupt.
    pub session: SessionIdDto,
}

/// Request corresponding to `session.set_permission_level`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetPermissionLevelRequest {
    /// Session whose permission level changes.
    pub session: SessionIdDto,
    /// New permission level.
    pub level: PermissionLevelDto,
}

/// Request corresponding to `session.set_reasoning_effort`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetReasoningEffortRequest {
    /// Session whose reasoning effort changes.
    pub session: SessionIdDto,
    /// New reasoning effort.
    pub effort: ReasoningEffortDto,
}

/// Error shared by RPCs backed by an open session.
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
pub enum SessionRpcError {
    /// `session.open` has not established a control handle.
    SessionNotOpen,
    /// The session lease is closed.
    SessionClosed,
    /// The active turn is not awaiting caller input.
    NotAwaitingInput,
    /// The response targets a different input request.
    InputRequestMismatch,
    /// The Agent runtime worker stopped.
    WorkerStopped,
    /// A streamed request was malformed.
    InvalidRequest,
}

/// Maximum session identifiers carried by one `session.list` item.
pub(crate) const MAX_SESSIONS_PER_LIST_ITEM: usize = 4;

/// One streamed item from `session.list`.
///
/// Carries up to [`MAX_SESSIONS_PER_LIST_ITEM`] session identifiers; `count`
/// records how many are valid. Unused slots serialize as zero identifiers.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListSessionsResponse {
    /// Number of valid session identifiers in `sessions`.
    pub count: u32,
    /// Session identifiers, padded with zero identifiers past `count`.
    pub sessions: [SessionIdDto; MAX_SESSIONS_PER_LIST_ITEM],
}

/// Request corresponding to `session.open`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenSessionRequest {
    /// Session to open.
    pub session: SessionIdDto,
}

/// Maximum UTF-8 bytes carried by one `session.append` or `session.respond`
/// message text.
pub(crate) const MESSAGE_TEXT_CAPACITY: usize = 400;

/// Request corresponding to `SessionControl::append`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppendRequestFrame {
    /// Session receiving the message.
    pub session: SessionIdDto,
    /// Message text.
    pub text: FixedStr<MESSAGE_TEXT_CAPACITY>,
}

/// Request corresponding to `SessionControl::respond`.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RespondRequestFrame {
    /// Session receiving the response.
    pub session: SessionIdDto,
    /// Input request being answered.
    pub request: InputRequestIdDto,
    /// Response text.
    pub text: FixedStr<MESSAGE_TEXT_CAPACITY>,
}

/// One streamed event from `session.open`.
///
/// `session` is the typed session identifier; `json` carries one complete
/// logical event as a JSON document.
#[repr(C)]
#[barracuda_event_router::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenSessionResponseFrame {
    /// Session that emitted the event.
    pub session: SessionIdDto,
    /// One logical event encoded as JSON.
    pub json: FixedStr<400>,
}

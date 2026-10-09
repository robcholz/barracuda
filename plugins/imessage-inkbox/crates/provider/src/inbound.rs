//! Inbound iMessages: the list request a receive loop polls and the fields
//! of each message it uses.
//!
//! `GET /api/v1/imessage/messages` returns a bare array, newest first. With
//! `start_datetime` (inclusive) it holds only messages created at or after
//! that instant, so a poll from the newest seen `created_at` repeats that
//! message and adds the new ones; callers dedup by `id`.

use alloc::string::String;
use alloc::vec::Vec;

use serde::Deserialize;

/// Messages one poll asks for.
pub const POLL_LIMIT: usize = 50;

/// One message of the list. Only the fields a receive loop uses are parsed.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct InboundMessage {
    /// Message UUID.
    pub id: String,
    /// Conversation UUID, the target the send path accepts.
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// `inbound` or `outbound`.
    #[serde(default)]
    pub direction: String,
    /// The other party's address.
    #[serde(default)]
    pub remote_number: Option<String>,
    /// Text body; `null` for media-only messages.
    #[serde(default)]
    pub content: Option<String>,
    /// Creation time (ISO 8601), the next poll's `start_datetime`.
    pub created_at: String,
}

impl InboundMessage {
    /// Whether the message came from the other party.
    #[must_use]
    pub fn is_inbound(&self) -> bool {
        self.direction == "inbound"
    }
}

/// The request target of one poll, below the API origin: newest first, at
/// most `limit` messages, created at or after `start` when given.
#[must_use]
pub fn messages_path(start: Option<&str>, limit: usize) -> String {
    let mut path = alloc::format!("/api/v1/imessage/messages?limit={limit}");
    if let Some(start) = start {
        path.push_str("&start_datetime=");
        for character in start.chars() {
            match character {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' => path.push(character),
                other => {
                    let mut bytes = [0_u8; 4];
                    for byte in other.encode_utf8(&mut bytes).bytes() {
                        path.push_str(&alloc::format!("%{byte:02X}"));
                    }
                }
            }
        }
    }
    path
}

/// Parses a list response body.
///
/// # Errors
///
/// Fails when the body is not a JSON array of messages.
pub fn parse_messages(body: &[u8]) -> Result<Vec<InboundMessage>, serde_json::Error> {
    serde_json::from_slice(body)
}

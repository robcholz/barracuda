//! A conversation's sessions, as a channel shows them after a session command.

use alloc::{format, string::String, vec::Vec};
use core::fmt::Write as _;

use serde::Serialize;

use crate::MessageTarget;

/// One session a conversation can switch to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionEntry {
    /// Agent session id, `session-N`.
    pub session: String,
    /// Human-facing title; `None` until the session's first message.
    pub title: Option<String>,
    /// Last use in Unix milliseconds, when the device clock knew it.
    pub updated_at: Option<u64>,
    /// Whether a turn is running in the session.
    pub running: bool,
}

/// What a session command did, for the channel to say.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionNotice {
    /// The conversation now continues `title`.
    Switched { title: Option<String> },
    /// The next message starts a new session; a temporary one is deleted when
    /// the conversation leaves it.
    Created { temporary: bool },
    /// A session was renamed.
    Renamed { title: String },
    /// A session was deleted.
    Deleted { title: Option<String> },
    /// Deleting asks first: the command repeats with `confirm`.
    ConfirmDelete { index: u32, title: Option<String> },
    /// The command named a session this conversation does not have.
    UnknownSession,
    /// A rename gave no usable title.
    InvalidTitle,
    /// The command was malformed.
    Usage,
    /// The device could not carry the command out.
    Failed,
}

/// A conversation's sessions after a session command, newest first.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SendSessionsRequest {
    /// The conversation the command came from.
    #[serde(skip)]
    pub target: MessageTarget,
    /// The session the conversation continues; `None` when its next message
    /// starts one.
    pub current: Option<String>,
    /// Whether the conversation is in a temporary session, or starts one with
    /// its next message. Temporary sessions are never listed.
    pub temporary: bool,
    /// Saved sessions, newest first; text commands number them from 1.
    pub sessions: Vec<SessionEntry>,
    /// What the command did, when it did more than list.
    pub notice: Option<SessionNotice>,
    /// The device's clock when the list was made, for relative times.
    pub now: Option<u64>,
}

/// The commands a text channel accepts, for usage replies and list footers.
pub const SESSION_COMMANDS: &str =
    "/sessions · /switch N · /new · /new temp · /rename N title · /delete N";

/// Renders a session command's reply as plain text (English; text channels
/// have no richer surface).
#[must_use]
pub fn sessions_text(request: &SendSessionsRequest) -> String {
    let named = |title: &Option<String>| match title {
        Some(title) => format!("“{title}”"),
        None => String::from("the new session"),
    };
    match &request.notice {
        Some(SessionNotice::Switched { title }) => return format!("Switched to {}.", named(title)),
        Some(SessionNotice::Created { temporary: false }) => {
            return String::from("Your next message starts a new session.");
        }
        Some(SessionNotice::Created { temporary: true }) => {
            return String::from(
                "Your next message starts a temporary chat. It is not saved and is deleted when you leave it.",
            );
        }
        Some(SessionNotice::Renamed { title }) => return format!("Renamed to “{title}”."),
        Some(SessionNotice::Deleted { title }) => return format!("Deleted {}.", named(title)),
        Some(SessionNotice::ConfirmDelete { index, title }) => {
            return format!(
                "Delete {}? This cannot be undone. Reply /delete {index} confirm.",
                named(title)
            );
        }
        Some(SessionNotice::UnknownSession) => {
            return String::from("No such session. Send /sessions to see the list.");
        }
        Some(SessionNotice::InvalidTitle) => {
            return String::from("Give the session a title: /rename N title");
        }
        Some(SessionNotice::Usage) => return format!("Commands: {SESSION_COMMANDS}"),
        Some(SessionNotice::Failed) => {
            return String::from("The device could not do that. Try again.");
        }
        None => {}
    }
    let mut text = String::new();
    if request.sessions.is_empty() {
        text.push_str("No saved sessions yet. Send a message to start one.");
    } else {
        text.push_str("Sessions, newest first (● current):");
        for (number, entry) in (1_u32..).zip(&request.sessions) {
            let current = request.current.as_deref() == Some(entry.session.as_str());
            let _ = write!(
                text,
                "\n{number}.{} {}",
                if current { " ●" } else { "" },
                entry.title.as_deref().unwrap_or("Untitled")
            );
            if entry.running {
                text.push_str(" · replying");
            }
            if let Some(age) = entry
                .updated_at
                .zip(request.now)
                .map(|(at, now)| ago(at, now))
            {
                let _ = write!(text, " · {age}");
            }
        }
    }
    if request.temporary {
        text.push_str("\nYou are in a temporary chat; it is not saved.");
    } else if request.current.is_none() && !request.sessions.is_empty() {
        text.push_str("\nYour next message starts a new session.");
    }
    let _ = write!(text, "\n{SESSION_COMMANDS}");
    text
}

/// How long ago `at` was, coarsely: the device knows no time zone.
fn ago(at: u64, now: u64) -> String {
    let minutes = now.saturating_sub(at) / 60_000;
    match minutes {
        0 => String::from("just now"),
        1..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} h ago", minutes / 60),
        _ => {
            let days = minutes / 1440;
            if days == 1 {
                String::from("yesterday")
            } else {
                format!("{days} days ago")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec};

    use super::{sessions_text, SendSessionsRequest, SessionEntry, SessionNotice};
    use crate::MessageTarget;

    fn list(notice: Option<SessionNotice>) -> SendSessionsRequest {
        SendSessionsRequest {
            target: MessageTarget::new("telegram", "chat-1"),
            current: Some(String::from("session-4")),
            temporary: false,
            sessions: vec![
                SessionEntry {
                    session: String::from("session-4"),
                    title: Some(String::from("Ride route")),
                    updated_at: Some(1_000_000_000 - 5 * 60_000),
                    running: true,
                },
                SessionEntry {
                    session: String::from("session-2"),
                    title: None,
                    updated_at: Some(1_000_000_000 - 30 * 3_600_000),
                    running: false,
                },
            ],
            notice,
            now: Some(1_000_000_000),
        }
    }

    #[test]
    fn a_list_numbers_sessions_newest_first_and_marks_the_current_one() {
        assert_eq!(
            sessions_text(&list(None)),
            "Sessions, newest first (● current):\n\
             1. ● Ride route · replying · 5 min ago\n\
             2. Untitled · yesterday\n\
             /sessions · /switch N · /new · /new temp · /rename N title · /delete N"
        );
    }

    #[test]
    fn a_notice_replaces_the_list() {
        let confirm = list(Some(SessionNotice::ConfirmDelete {
            index: 2,
            title: Some(String::from("Ride route")),
        }));
        assert_eq!(
            sessions_text(&confirm),
            "Delete “Ride route”? This cannot be undone. Reply /delete 2 confirm."
        );
        let created = list(Some(SessionNotice::Created { temporary: true }));
        assert!(sessions_text(&created).starts_with("Your next message starts a temporary chat."));
    }

    #[test]
    fn an_empty_conversation_says_how_to_start() {
        let mut empty = list(None);
        empty.sessions.clear();
        empty.current = None;
        assert!(sessions_text(&empty).starts_with("No saved sessions yet."));
    }
}

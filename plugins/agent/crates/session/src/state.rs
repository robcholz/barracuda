//! Durable state owned by the Session subsystem.

use alloc::string::String;

use barracuda_agent_permission::PermissionLevel;
use barracuda_agent_persistence::DurableState;
use serde::{Deserialize, Serialize};

use barracuda_agent::{AgentId, AgentIdAllocator, ReasoningEffort};

use super::manager::SessionId;

barracuda_runtime_utils::define_id_allocator!(
    /// Hands out process-unique session ids for the current runtime.
    pub(super) SessionIdAllocator(SessionId),
    SessionId(1)
);

#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct SessionManagerState {
    agent_id_allocator: AgentIdAllocator,
    session_id_allocator: SessionIdAllocator,
}

#[derive(Clone, Debug)]
pub(super) struct AgentIdAllocatorHandle {
    state: DurableState<SessionManagerState>,
}

impl AgentIdAllocatorHandle {
    pub(super) fn new(state: &DurableState<SessionManagerState>) -> Self {
        Self {
            state: state.clone(),
        }
    }

    pub(super) fn next(&self) -> AgentId {
        self.state.get_mut().agent_id_allocator.next()
    }
}

pub(super) fn allocate_session_id(state: &DurableState<SessionManagerState>) -> SessionId {
    state.get_mut().session_id_allocator.next()
}

pub(super) fn ensure_next_session_id(state: &DurableState<SessionManagerState>, next: SessionId) {
    let mut state = state.get_mut();
    if state.session_id_allocator.peek() < next {
        state.session_id_allocator = SessionIdAllocator::starting_at(next);
    }
}

pub(super) fn ensure_next_agent_id(state: &DurableState<SessionManagerState>, next: AgentId) {
    let mut state = state.get_mut();
    if state.agent_id_allocator.peek() < next {
        state.agent_id_allocator = AgentIdAllocator::starting_at(next);
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(super) struct SessionPersistentState {
    pub(super) reasoning_effort: ReasoningEffort,
    pub(super) permission_level: PermissionLevel,
    pub(super) root_agent: Option<AgentId>,
    /// Human-facing title, derived from the first user message or renamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) title: Option<String>,
    /// Unix milliseconds of the last appended user message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) updated_at: Option<u64>,
}

impl SessionPersistentState {
    pub(super) fn clear_root(&mut self) {
        self.root_agent = None;
    }
}

/// Longest title, in Unicode scalar values, including a truncation marker.
const TITLE_MAX_CHARS: usize = 64;
/// Characters kept before the truncation marker of an over-long title.
const TITLE_TRUNCATED_CHARS: usize = 63;
const TITLE_TRUNCATION_MARKER: char = '\u{2026}';

/// Normalizes text into a Session title.
///
/// Keeps the first non-empty line, trimmed, capped at [`TITLE_MAX_CHARS`]
/// characters with a trailing ellipsis when cut. Returns `None` when no line
/// has visible text.
pub(super) fn normalize_title(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    if line.chars().nth(TITLE_MAX_CHARS).is_none() {
        return Some(line.into());
    }
    let mut title: String = line.chars().take(TITLE_TRUNCATED_CHARS).collect();
    title.push(TITLE_TRUNCATION_MARKER);
    Some(title)
}

/// Records Session metadata for one appended user message.
///
/// An untitled Session takes its title from `text`; `now`, when known,
/// becomes the last-used time. State is only marked dirty when it changes.
pub(super) fn record_user_message(
    state: &DurableState<SessionPersistentState>,
    text: &str,
    now: Option<u64>,
) {
    let title = if state.get().title.is_none() {
        normalize_title(text)
    } else {
        None
    };
    if title.is_none() && now.is_none() {
        return;
    }
    let mut state = state.get_mut();
    if title.is_some() {
        state.title = title;
    }
    if now.is_some() {
        state.updated_at = now;
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use barracuda_agent_persistence::DurableState;

    use super::{
        allocate_session_id, ensure_next_agent_id, ensure_next_session_id, normalize_title,
        record_user_message, AgentIdAllocatorHandle, SessionManagerState, SessionPersistentState,
    };
    use crate::SessionId;
    use barracuda_agent::AgentId;

    #[test]
    fn manager_state_owns_both_global_allocators() {
        let state = DurableState::new(SessionManagerState::default());

        ensure_next_session_id(&state, SessionId::new(4));
        ensure_next_agent_id(&state, AgentId::new(7));

        assert_eq!(allocate_session_id(&state), SessionId::new(4));
        assert_eq!(AgentIdAllocatorHandle::new(&state).next(), AgentId::new(7));
    }

    #[test]
    fn title_keeps_the_first_non_empty_trimmed_line() {
        assert_eq!(
            normalize_title("\n  \t\r\n  Plan the trip  \nsecond line").as_deref(),
            Some("Plan the trip")
        );
    }

    #[test]
    fn blank_text_has_no_title() {
        assert_eq!(normalize_title(""), None);
        assert_eq!(normalize_title(" \n\t \r\n "), None);
    }

    #[test]
    fn title_is_capped_at_sixty_four_characters_with_an_ellipsis() {
        let exact: String = "\u{e9}".repeat(64);
        assert_eq!(normalize_title(&exact).as_deref(), Some(exact.as_str()));

        let long: String = "\u{e9}".repeat(65);
        let title = normalize_title(&long).unwrap_or_default();
        assert_eq!(title.chars().count(), 64);
        assert!(title.ends_with('\u{2026}'));
        assert_eq!(title.chars().filter(|c| *c == '\u{e9}').count(), 63);
    }

    #[test]
    fn first_message_sets_the_title_and_later_messages_keep_it() {
        let state = DurableState::new(SessionPersistentState::default());

        record_user_message(&state, "  \n", None);
        assert_eq!(state.get().title, None);

        record_user_message(&state, "first question\nmore detail", None);
        record_user_message(&state, "second question", None);
        assert_eq!(state.get().title.as_deref(), Some("first question"));
    }

    #[test]
    fn known_time_stamps_updated_at_and_unknown_time_keeps_it() {
        let state = DurableState::new(SessionPersistentState::default());

        record_user_message(&state, "hello", None);
        assert_eq!(state.get().updated_at, None);

        record_user_message(&state, "hello", Some(1_000));
        assert_eq!(state.get().updated_at, Some(1_000));

        record_user_message(&state, "again", None);
        assert_eq!(state.get().updated_at, Some(1_000));

        record_user_message(&state, "", Some(2_000));
        assert_eq!(state.get().updated_at, Some(2_000));
        assert_eq!(state.get().title.as_deref(), Some("hello"));
    }
}

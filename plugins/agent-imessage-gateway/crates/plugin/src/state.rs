use alloc::{collections::VecDeque, string::String, vec::Vec};
use core::mem::size_of;

use barracuda_plugin::manager::PluginStorage;
use embassy_time::{Duration, Instant};
use serde::Serialize;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

pub(crate) const CHANNEL_MAX: usize = 32;
pub(crate) const CONVERSATION_MAX: usize = 96;
pub(crate) const THREAD_MAX: usize = 64;
pub(crate) const MESSAGE_ID_MAX: usize = 96;
pub(crate) const SESSION_MAX: usize = 32;
/// Version 2 adds the `current` flag; version 1 records had one session per
/// route, which is current.
const RECORD_VERSION: u32 = 2;
const FLAG_CURRENT: u16 = 1;
/// How long a route stays reserved for the inbound message that found it
/// unmapped, while that message's Workflow creates and binds a session.
pub(crate) const RESERVATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Route {
    pub(crate) channel: String,
    pub(crate) conversation_id: String,
    pub(crate) thread_id: Option<String>,
}

impl Route {
    pub(crate) fn new(
        channel: &str,
        conversation_id: &str,
        thread_id: Option<&str>,
    ) -> Result<Self, BridgeError> {
        if !valid_text(channel, CHANNEL_MAX)
            || !valid_text(conversation_id, CONVERSATION_MAX)
            || thread_id.is_some_and(|value| !valid_text(value, THREAD_MAX))
        {
            return Err(BridgeError::InvalidRequest);
        }
        Ok(Self {
            channel: String::from(channel),
            conversation_id: String::from(conversation_id),
            thread_id: thread_id.map(String::from),
        })
    }
}

/// One session a route has used. A route may own several sessions; at most
/// one is current, and only the current one receives the route's messages
/// and reaches it with output.
#[derive(Clone, Debug)]
pub(crate) struct Mapping {
    session: String,
    route: Route,
    opened: bool,
    /// The route's messages go to this session.
    current: bool,
    /// Never saved; deleted when the route leaves it.
    temporary: bool,
    pending_replies: VecDeque<String>,
    active_turn: Option<ActiveTurn>,
}

impl Mapping {
    pub(crate) fn session(&self) -> &str {
        &self.session
    }

    pub(crate) const fn temporary(&self) -> bool {
        self.temporary
    }

    /// Whether a turn is running in the session, as its events last said.
    pub(crate) const fn running(&self) -> bool {
        self.active_turn.is_some()
    }
}

#[derive(Clone, Debug)]
struct ActiveTurn {
    reply_to: Option<String>,
    /// The Agent's open input request (`input-N`), answered by the next
    /// inbound message instead of that message starting a turn.
    input_request: Option<String>,
    /// The turn started while the session was its route's current one, so it
    /// streams to the route until it ends, even after the route leaves it.
    delivered: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolveResult {
    Missing,
    Found {
        session: String,
        open_required: bool,
    },
}

/// Outcome of [`BridgeBook::resolve_or_reserve`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Resolution {
    /// The route is mapped, or was unmapped and is now reserved for the
    /// caller ([`ResolveResult::Missing`]), which must create and bind a session.
    Ready(ResolveResult),
    /// Another inbound message reserved the unmapped route; resolve again
    /// once the book changes or at `until`, when the reservation lapses.
    Wait { until: Instant },
}

/// An unmapped route whose first inbound message is creating its session.
#[derive(Clone, Debug)]
struct Reservation {
    route: Route,
    expires: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GatewayTarget {
    pub(crate) route: Route,
    pub(crate) reply_to: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GatewayEvent {
    UserTurnStarted,
    OtherTurnStarted,
    TurnEnded,
    Closed,
    Continuing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BridgeError {
    InvalidRequest,
    Conflict,
    Storage,
}

impl BridgeError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Conflict => "conflict",
            Self::Storage => "storage",
        }
    }
}

/// Bindings between conversation routes and agent sessions.
///
/// Each session has at most one route; a route owns the sessions it started
/// and has at most one current session. One list of mappings serves lookups
/// from either side; a device holds few.
///
/// An unmapped route is reserved by the first inbound message that resolves
/// it, so messages arriving while that message's Workflow creates the session
/// wait and then bind to the same session instead of creating another.
pub(crate) struct BridgeBook {
    mappings: Vec<Mapping>,
    reservations: Vec<Reservation>,
    /// Routes whose next session is temporary.
    temporary_next: Vec<Route>,
}

impl BridgeBook {
    pub(crate) const fn new() -> Self {
        Self {
            mappings: Vec::new(),
            reservations: Vec::new(),
            temporary_next: Vec::new(),
        }
    }

    fn by_session(&self, session: &str) -> Option<&Mapping> {
        self.mappings
            .iter()
            .find(|mapping| mapping.session == session)
    }

    /// The route's current session.
    pub(crate) fn current_of(&self, route: &Route) -> Option<&Mapping> {
        self.mappings
            .iter()
            .find(|mapping| mapping.current && mapping.route == *route)
    }

    /// Every session the route owns, current or not.
    pub(crate) fn sessions_of<'a>(&'a self, route: &'a Route) -> impl Iterator<Item = &'a Mapping> {
        self.mappings
            .iter()
            .filter(move |mapping| mapping.route == *route)
    }

    /// Whether the route's next session is temporary.
    pub(crate) fn temporary_next(&self, route: &Route) -> bool {
        self.temporary_next.contains(route)
    }

    /// Leaves the route's current session, if any: the route's next message
    /// starts a session, temporary when `temporary`. Returns the session left;
    /// a temporary one is forgotten, for the caller to delete.
    pub(crate) fn leave(&mut self, route: &Route, temporary: bool) -> Option<Mapping> {
        self.temporary_next.retain(|next| next != route);
        if temporary {
            self.temporary_next.push(route.clone());
        }
        let index = self
            .mappings
            .iter()
            .position(|mapping| mapping.current && mapping.route == *route)?;
        if self
            .mappings
            .get(index)
            .is_some_and(|mapping| mapping.temporary)
        {
            return Some(self.mappings.swap_remove(index));
        }
        let mapping = self.mappings.get_mut(index)?;
        mapping.current = false;
        Some(mapping.clone())
    }

    /// Makes `session`, one of the route's sessions, its current one. The
    /// caller leaves the previous current session first.
    pub(crate) fn make_current(
        &mut self,
        route: &Route,
        session: &str,
    ) -> Result<Mapping, BridgeError> {
        if self
            .current_of(route)
            .is_some_and(|mapping| mapping.session != session)
        {
            return Err(BridgeError::Conflict);
        }
        let mapping = self
            .mappings
            .iter_mut()
            .find(|mapping| mapping.session == session && mapping.route == *route)
            .ok_or(BridgeError::InvalidRequest)?;
        mapping.current = true;
        self.temporary_next.retain(|next| next != route);
        Ok(mapping.clone())
    }

    /// Forgets a deleted session. Its route, if it was current there, starts
    /// a new session with its next message.
    pub(crate) fn remove(&mut self, session: &str) -> Option<Mapping> {
        let index = self
            .mappings
            .iter()
            .position(|mapping| mapping.session == session)?;
        Some(self.mappings.swap_remove(index))
    }

    fn insert(&mut self, mapping: Mapping) {
        self.mappings.reserve_exact(1);
        self.mappings.push(mapping);
    }

    pub(crate) fn restore(
        &mut self,
        session: &str,
        persisted: PersistedRoute,
    ) -> Result<(), BridgeError> {
        if !valid_session(session) || self.by_session(session).is_some() {
            return Err(BridgeError::InvalidRequest);
        }
        let mut mapping = persisted.into_mapping(session)?;
        mapping.opened = false;
        // a route has one current session; a second record claiming it yields
        mapping.current = mapping.current && self.current_of(&mapping.route).is_none();
        self.insert(mapping);
        Ok(())
    }

    pub(crate) fn resolve(&self, route: &Route) -> ResolveResult {
        let Some(mapping) = self.current_of(route) else {
            return ResolveResult::Missing;
        };
        ResolveResult::Found {
            session: mapping.session.clone(),
            open_required: !mapping.opened,
        }
    }

    /// Resolves `route`, reserving it for the caller when it is unmapped and
    /// not already reserved by another inbound message.
    ///
    /// A reservation lasts until [`commit_mapping`](Self::commit_mapping) or
    /// [`release`](Self::release) for the route, or until it lapses after
    /// [`RESERVATION_TIMEOUT`] because the reserving Workflow never bound a
    /// session; the next caller then takes it over.
    pub(crate) fn resolve_or_reserve(&mut self, route: &Route, now: Instant) -> Resolution {
        let found = self.resolve(route);
        if found != ResolveResult::Missing {
            return Resolution::Ready(found);
        }
        self.reservations
            .retain(|reservation| reservation.expires > now);
        if let Some(reservation) = self
            .reservations
            .iter()
            .find(|reservation| reservation.route == *route)
        {
            return Resolution::Wait {
                until: reservation.expires,
            };
        }
        self.reservations.reserve_exact(1);
        self.reservations.push(Reservation {
            route: route.clone(),
            expires: now.saturating_add(RESERVATION_TIMEOUT),
        });
        Resolution::Ready(ResolveResult::Missing)
    }

    /// Drops the reservation of `route`, letting a waiting message take it.
    pub(crate) fn release(&mut self, route: &Route) {
        self.reservations
            .retain(|reservation| reservation.route != *route);
    }

    /// Stores a mapping from [`prepare_binding`](Self::prepare_binding),
    /// replacing the session's previous state and ending the route's
    /// reservation.
    pub(crate) fn commit_mapping(&mut self, mapping: Mapping) {
        self.release(&mapping.route);
        if mapping.current {
            self.temporary_next.retain(|next| *next != mapping.route);
        }
        match self
            .mappings
            .iter_mut()
            .find(|existing| existing.session == mapping.session)
        {
            Some(existing) => *existing = mapping,
            None => self.insert(mapping),
        }
    }

    /// Prepares the session's mapping for one inbound message. When the
    /// session's turn waits on an input request, the message answers it: the
    /// request id is returned and no reply is queued, since no turn starts.
    pub(crate) fn prepare_binding(
        &self,
        route: Route,
        message_id: &str,
        session: &str,
    ) -> Result<(Mapping, Option<String>), BridgeError> {
        if !valid_text(message_id, MESSAGE_ID_MAX) || !valid_session(session) {
            return Err(BridgeError::InvalidRequest);
        }
        if self
            .current_of(&route)
            .is_some_and(|existing| existing.session != session)
            || self
                .by_session(session)
                .is_some_and(|existing| existing.route != route)
        {
            return Err(BridgeError::Conflict);
        }
        let temporary = self.temporary_next(&route);
        let mut mapping = self.by_session(session).cloned().unwrap_or(Mapping {
            session: String::from(session),
            route,
            opened: true,
            current: true,
            temporary,
            pending_replies: VecDeque::new(),
            active_turn: None,
        });
        mapping.opened = true;
        mapping.current = true;
        let input_request = mapping
            .active_turn
            .as_mut()
            .and_then(|turn| turn.input_request.take());
        if input_request.is_none() {
            mapping.pending_replies.push_back(String::from(message_id));
        }
        Ok((mapping, input_request))
    }

    /// Records that the session's active turn waits on input request `request`.
    pub(crate) fn input_requested(&mut self, session: &str, request: String) {
        if let Some(turn) = self
            .mappings
            .iter_mut()
            .find(|mapping| mapping.session == session)
            .and_then(|mapping| mapping.active_turn.as_mut())
        {
            turn.input_request = Some(request);
        }
    }

    pub(crate) fn gateway_target(
        &mut self,
        session: &str,
        event: GatewayEvent,
    ) -> Result<Option<GatewayTarget>, BridgeError> {
        if !valid_session(session) {
            return Err(BridgeError::InvalidRequest);
        }
        let Some(mapping) = self
            .mappings
            .iter_mut()
            .find(|mapping| mapping.session == session)
        else {
            return Ok(None);
        };
        match event {
            GatewayEvent::Closed => {
                mapping.opened = false;
                mapping.pending_replies.clear();
                mapping.active_turn = None;
                return Ok(None);
            }
            GatewayEvent::UserTurnStarted => {
                if mapping.active_turn.is_some() {
                    return Err(BridgeError::Conflict);
                }
                let reply_to = mapping
                    .pending_replies
                    .pop_front()
                    .ok_or(BridgeError::InvalidRequest)?;
                mapping.active_turn = Some(ActiveTurn {
                    reply_to: Some(reply_to),
                    input_request: None,
                    delivered: mapping.current,
                });
            }
            GatewayEvent::OtherTurnStarted => {
                if mapping.active_turn.is_some() {
                    return Err(BridgeError::Conflict);
                }
                mapping.active_turn = Some(ActiveTurn {
                    reply_to: None,
                    input_request: None,
                    delivered: mapping.current,
                });
            }
            GatewayEvent::TurnEnded | GatewayEvent::Continuing => {}
        }
        // a turn the route started finishes there; a later one in a left session reaches no one
        let target = mapping
            .active_turn
            .as_ref()
            .filter(|active| active.delivered)
            .map(|active| GatewayTarget {
                route: mapping.route.clone(),
                reply_to: active.reply_to.clone(),
            });
        if event == GatewayEvent::TurnEnded {
            mapping.active_turn = None;
        }
        Ok(target)
    }
}

fn valid_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max
}

pub(crate) fn validate_message_id(value: &str) -> Result<(), BridgeError> {
    if valid_text(value, MESSAGE_ID_MAX) {
        Ok(())
    } else {
        Err(BridgeError::InvalidRequest)
    }
}

fn valid_session(value: &str) -> bool {
    value.len() <= SESSION_MAX
        && value.strip_prefix("session-").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[repr(C)]
#[derive(Clone, Copy, Immutable, IntoBytes, KnownLayout, TryFromBytes)]
pub(crate) struct PersistedRoute {
    version: u32,
    channel_len: u16,
    conversation_len: u16,
    thread_len: u16,
    /// [`FLAG_CURRENT`]. Version 1 kept a zero length here for reply data
    /// that became runtime-only.
    flags: u16,
    channel: [u8; CHANNEL_MAX],
    conversation_id: [u8; CONVERSATION_MAX],
    thread_id: [u8; THREAD_MAX],
    reserved: [u8; MESSAGE_ID_MAX],
}

impl PersistedRoute {
    pub(crate) fn from_mapping(mapping: &Mapping) -> Result<Self, BridgeError> {
        let mut record = Self {
            version: RECORD_VERSION,
            channel_len: u16::try_from(mapping.route.channel.len())
                .map_err(|_error| BridgeError::InvalidRequest)?,
            conversation_len: u16::try_from(mapping.route.conversation_id.len())
                .map_err(|_error| BridgeError::InvalidRequest)?,
            thread_len: u16::try_from(mapping.route.thread_id.as_deref().map_or(0, str::len))
                .map_err(|_error| BridgeError::InvalidRequest)?,
            flags: if mapping.current { FLAG_CURRENT } else { 0 },
            channel: [0; CHANNEL_MAX],
            conversation_id: [0; CONVERSATION_MAX],
            thread_id: [0; THREAD_MAX],
            reserved: [0; MESSAGE_ID_MAX],
        };
        copy_text(&mapping.route.channel, &mut record.channel)?;
        copy_text(&mapping.route.conversation_id, &mut record.conversation_id)?;
        if let Some(thread_id) = &mapping.route.thread_id {
            copy_text(thread_id, &mut record.thread_id)?;
        }
        Ok(record)
    }

    fn into_mapping(self, session: &str) -> Result<Mapping, BridgeError> {
        let current = match self.version {
            1 => true,
            RECORD_VERSION => self.flags & FLAG_CURRENT != 0,
            _ => return Err(BridgeError::InvalidRequest),
        };
        let channel = read_text(&self.channel, self.channel_len)?;
        let conversation_id = read_text(&self.conversation_id, self.conversation_len)?;
        let thread_id = if self.thread_len == 0 {
            None
        } else {
            Some(read_text(&self.thread_id, self.thread_len)?)
        };
        let route = Route::new(&channel, &conversation_id, thread_id.as_deref())?;
        Ok(Mapping {
            session: String::from(session),
            route,
            opened: false,
            current,
            temporary: false,
            pending_replies: VecDeque::new(),
            active_turn: None,
        })
    }
}

const _: () = assert!(size_of::<PersistedRoute>() == 300);

fn copy_text(value: &str, destination: &mut [u8]) -> Result<(), BridgeError> {
    let target = destination
        .get_mut(..value.len())
        .ok_or(BridgeError::InvalidRequest)?;
    target.copy_from_slice(value.as_bytes());
    Ok(())
}

fn read_text(source: &[u8], length: u16) -> Result<String, BridgeError> {
    let bytes = source
        .get(..usize::from(length))
        .ok_or(BridgeError::InvalidRequest)?;
    let value = core::str::from_utf8(bytes).map_err(|_error| BridgeError::InvalidRequest)?;
    if value.is_empty() {
        return Err(BridgeError::InvalidRequest);
    }
    Ok(String::from(value))
}

/// Saves a mapping; a temporary session's mapping is never saved.
pub(crate) async fn persist_mapping<Storage: PluginStorage>(
    storage: &Storage,
    mapping: &Mapping,
) -> Result<(), BridgeError> {
    if mapping.temporary {
        return Ok(());
    }
    let record = PersistedRoute::from_mapping(mapping)?;
    storage
        .put(&mapping.session, &record)
        .await
        .map_err(|_error| BridgeError::Storage)
}

/// Forgets a saved mapping.
pub(crate) async fn forget_mapping<Storage: PluginStorage>(
    storage: &Storage,
    session: &str,
) -> Result<(), BridgeError> {
    storage
        .delete(session)
        .await
        .map_err(|_error| BridgeError::Storage)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::string::String;

    use embassy_time::{Duration, Instant};

    use super::{
        BridgeBook, GatewayEvent, Mapping, PersistedRoute, RESERVATION_TIMEOUT, Resolution,
        ResolveResult, Route,
    };

    fn route() -> Route {
        Route::new("imessage", "chat-7", Some("thread-2")).expect("valid route")
    }

    #[test]
    fn an_unmapped_route_is_reserved_for_its_first_message_until_bound() {
        let mut book = BridgeBook::new();
        let now = Instant::from_secs(100);
        let expires = now.saturating_add(RESERVATION_TIMEOUT);

        assert_eq!(
            book.resolve_or_reserve(&route(), now),
            Resolution::Ready(ResolveResult::Missing)
        );
        assert_eq!(
            book.resolve_or_reserve(&route(), now.saturating_add(Duration::from_secs(1))),
            Resolution::Wait { until: expires }
        );
        let other = Route::new("inkbox", "chat-7", None).expect("valid route");
        assert_eq!(
            book.resolve_or_reserve(&other, now),
            Resolution::Ready(ResolveResult::Missing),
            "reservations are per route"
        );

        let mapping = book
            .prepare_binding(route(), "message-9", "session-4")
            .expect("prepare binding")
            .0;
        book.commit_mapping(mapping);
        assert_eq!(
            book.resolve_or_reserve(&route(), now),
            Resolution::Ready(ResolveResult::Found {
                session: String::from("session-4"),
                open_required: false,
            })
        );
    }

    #[test]
    fn a_released_or_lapsed_reservation_passes_to_the_next_message() {
        let mut book = BridgeBook::new();
        let now = Instant::from_secs(100);
        let _first = book.resolve_or_reserve(&route(), now);

        book.release(&route());
        assert_eq!(
            book.resolve_or_reserve(&route(), now),
            Resolution::Ready(ResolveResult::Missing),
            "a failed bind hands the route to a waiting message"
        );
        assert!(matches!(
            book.resolve_or_reserve(&route(), now),
            Resolution::Wait { .. }
        ));
        let lapsed = now.saturating_add(RESERVATION_TIMEOUT);
        assert_eq!(
            book.resolve_or_reserve(&route(), lapsed),
            Resolution::Ready(ResolveResult::Missing),
            "a Workflow that never binds cannot hold the route"
        );
        assert_eq!(
            book.resolve_or_reserve(&route(), lapsed),
            Resolution::Wait {
                until: lapsed.saturating_add(RESERVATION_TIMEOUT)
            }
        );
    }

    #[test]
    fn queued_replies_are_consumed_in_user_turn_order() {
        let mut book = BridgeBook::new();
        let first = book
            .prepare_binding(route(), "message-9", "session-4")
            .expect("prepare first binding")
            .0;
        book.commit_mapping(first);
        let second = book
            .prepare_binding(route(), "message-10", "session-4")
            .expect("prepare second binding")
            .0;
        book.commit_mapping(second);

        assert_eq!(
            book.resolve(&route()),
            ResolveResult::Found {
                session: String::from("session-4"),
                open_required: false,
            }
        );
        let first_target = book
            .gateway_target("session-4", GatewayEvent::UserTurnStarted)
            .expect("start first turn")
            .expect("first target");
        assert_eq!(first_target.reply_to.as_deref(), Some("message-9"));
        let continuing = book
            .gateway_target("session-4", GatewayEvent::Continuing)
            .expect("continue first turn")
            .expect("continuing target");
        assert_eq!(continuing.reply_to.as_deref(), Some("message-9"));
        let ended = book
            .gateway_target("session-4", GatewayEvent::TurnEnded)
            .expect("end first turn")
            .expect("terminal target");
        assert_eq!(ended.reply_to.as_deref(), Some("message-9"));

        let second_target = book
            .gateway_target("session-4", GatewayEvent::UserTurnStarted)
            .expect("start second turn")
            .expect("second target");
        assert_eq!(second_target.reply_to.as_deref(), Some("message-10"));
    }

    #[test]
    fn closing_and_restoring_require_the_agent_session_to_reopen() {
        let mut book = BridgeBook::new();
        let mapping = book
            .prepare_binding(route(), "message-9", "session-4")
            .expect("prepare binding")
            .0;
        let persisted = PersistedRoute::from_mapping(&mapping).expect("encode persisted route");
        book.commit_mapping(mapping);
        assert_eq!(
            book.gateway_target("session-4", GatewayEvent::Closed)
                .expect("close session"),
            None
        );
        assert_eq!(
            book.resolve(&route()),
            ResolveResult::Found {
                session: String::from("session-4"),
                open_required: true,
            }
        );

        let mut restored = BridgeBook::new();
        restored
            .restore("session-4", persisted)
            .expect("restore route");
        assert_eq!(
            restored.resolve(&route()),
            ResolveResult::Found {
                session: String::from("session-4"),
                open_required: true,
            }
        );
    }

    #[test]
    fn tool_turns_keep_the_route_without_consuming_a_reply() {
        let mut book = BridgeBook::new();
        let mapping = book
            .prepare_binding(route(), "message-9", "session-4")
            .expect("prepare binding")
            .0;
        book.commit_mapping(mapping);

        let tool_target = book
            .gateway_target("session-4", GatewayEvent::OtherTurnStarted)
            .expect("start tool turn")
            .expect("tool target");
        assert_eq!(tool_target.route, route());
        assert_eq!(tool_target.reply_to, None);
        let _ended = book
            .gateway_target("session-4", GatewayEvent::TurnEnded)
            .expect("end tool turn");
        let user_target = book
            .gateway_target("session-4", GatewayEvent::UserTurnStarted)
            .expect("start user turn")
            .expect("user target");
        assert_eq!(user_target.reply_to.as_deref(), Some("message-9"));
    }

    #[test]
    fn a_message_during_an_input_request_answers_it_without_queuing_a_reply() {
        let mut book = BridgeBook::new();
        let (mapping, request) = book
            .prepare_binding(route(), "message-9", "session-4")
            .expect("prepare binding");
        assert_eq!(request, None);
        book.commit_mapping(mapping);
        let _started = book
            .gateway_target("session-4", GatewayEvent::UserTurnStarted)
            .expect("start turn");
        book.input_requested("session-4", String::from("input-3"));

        let (answer, request) = book
            .prepare_binding(route(), "message-10", "session-4")
            .expect("prepare answer");
        assert_eq!(request.as_deref(), Some("input-3"));
        assert!(answer.pending_replies.is_empty());
        book.commit_mapping(answer);

        let continuing = book
            .gateway_target("session-4", GatewayEvent::Continuing)
            .expect("continue turn")
            .expect("continuing target");
        assert_eq!(continuing.reply_to.as_deref(), Some("message-9"));
        let (_next, request) = book
            .prepare_binding(route(), "message-11", "session-4")
            .expect("prepare next message");
        assert_eq!(request, None, "the request is answered once");
    }

    #[test]
    fn a_left_session_finishes_its_turn_then_reaches_no_one() {
        let mut book = BridgeBook::new();
        let mapping = book
            .prepare_binding(route(), "message-9", "session-4")
            .expect("prepare binding")
            .0;
        book.commit_mapping(mapping);
        let _started = book
            .gateway_target("session-4", GatewayEvent::UserTurnStarted)
            .expect("start turn");

        let left = book
            .leave(&route(), false)
            .expect("leave the current session");
        assert!(left.running(), "its turn still runs on the device");
        assert_eq!(book.resolve(&route()), ResolveResult::Missing);
        assert!(
            book.gateway_target("session-4", GatewayEvent::TurnEnded)
                .expect("end")
                .is_some(),
            "the turn the route asked for still finishes there"
        );
        assert_eq!(
            book.gateway_target("session-4", GatewayEvent::OtherTurnStarted)
                .expect("start a tool turn"),
            None,
            "a later turn of a left session reaches no one"
        );
        let _ended = book.gateway_target("session-4", GatewayEvent::TurnEnded);

        // switching back makes it current again; a new route session would conflict
        book.make_current(&route(), "session-4")
            .expect("switch back");
        assert!(
            book.gateway_target("session-4", GatewayEvent::OtherTurnStarted)
                .expect("start")
                .is_some()
        );
        assert!(book.make_current(&route(), "session-5").is_err());
    }

    #[test]
    fn a_temporary_session_is_unsaved_and_forgotten_when_left() {
        let mut book = BridgeBook::new();
        let _left = book.leave(&route(), true);
        assert!(book.temporary_next(&route()));
        let mapping = book
            .prepare_binding(route(), "message-9", "session-6")
            .expect("prepare binding")
            .0;
        assert!(mapping.temporary());
        book.commit_mapping(mapping);
        assert!(!book.temporary_next(&route()), "only the next session");

        let left = book.leave(&route(), false).expect("leave");
        assert_eq!(left.session(), "session-6");
        assert!(left.temporary());
        assert_eq!(
            book.sessions_of(&route()).count(),
            0,
            "forgotten on the way out"
        );
    }

    #[test]
    fn version_one_records_restore_as_the_current_session() {
        let book = BridgeBook::new();
        let mapping = book
            .prepare_binding(route(), "message-9", "session-4")
            .expect("prepare binding")
            .0;
        let mut record = PersistedRoute::from_mapping(&mapping).expect("encode");
        record.version = 1;
        record.flags = 0;
        let second = PersistedRoute::from_mapping(&mapping).expect("encode");

        let mut restored = BridgeBook::new();
        restored.restore("session-4", record).expect("restore v1");
        restored.restore("session-5", second).expect("restore v2");
        assert_eq!(
            restored.current_of(&route()).map(Mapping::session),
            Some("session-4"),
            "a route keeps one current session"
        );
        assert_eq!(restored.sessions_of(&route()).count(), 2);
    }
}

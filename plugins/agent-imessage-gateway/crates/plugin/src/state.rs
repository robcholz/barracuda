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
const RECORD_VERSION: u32 = 1;
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

#[derive(Clone, Debug)]
pub(crate) struct Mapping {
    session: String,
    route: Route,
    opened: bool,
    pending_replies: VecDeque<String>,
    active_turn: Option<ActiveTurn>,
}

#[derive(Clone, Debug)]
struct ActiveTurn {
    reply_to: Option<String>,
    /// The Agent's open input request (`input-N`), answered by the next
    /// inbound message instead of that message starting a turn.
    input_request: Option<String>,
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
/// Each session has at most one route and each route at most one session, so
/// one list of mappings serves lookups from either side; a device holds few.
///
/// An unmapped route is reserved by the first inbound message that resolves
/// it, so messages arriving while that message's Workflow creates the session
/// wait and then bind to the same session instead of creating another.
pub(crate) struct BridgeBook {
    mappings: Vec<Mapping>,
    reservations: Vec<Reservation>,
}

impl BridgeBook {
    pub(crate) const fn new() -> Self {
        Self {
            mappings: Vec::new(),
            reservations: Vec::new(),
        }
    }

    fn by_session(&self, session: &str) -> Option<&Mapping> {
        self.mappings
            .iter()
            .find(|mapping| mapping.session == session)
    }

    fn by_route(&self, route: &Route) -> Option<&Mapping> {
        self.mappings.iter().find(|mapping| mapping.route == *route)
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
        if self.by_route(&mapping.route).is_some() {
            return Err(BridgeError::Conflict);
        }
        self.insert(mapping);
        Ok(())
    }

    pub(crate) fn resolve(&self, route: &Route) -> ResolveResult {
        let Some(mapping) = self.by_route(route) else {
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
            .by_route(&route)
            .is_some_and(|existing| existing.session != session)
            || self
                .by_session(session)
                .is_some_and(|existing| existing.route != route)
        {
            return Err(BridgeError::Conflict);
        }
        let mut mapping = self.by_session(session).cloned().unwrap_or(Mapping {
            session: String::from(session),
            route,
            opened: true,
            pending_replies: VecDeque::new(),
            active_turn: None,
        });
        mapping.opened = true;
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
                });
            }
            GatewayEvent::OtherTurnStarted => {
                if mapping.active_turn.is_some() {
                    return Err(BridgeError::Conflict);
                }
                mapping.active_turn = Some(ActiveTurn {
                    reply_to: None,
                    input_request: None,
                });
            }
            GatewayEvent::TurnEnded | GatewayEvent::Continuing => {}
        }
        let target = mapping.active_turn.as_ref().map(|active| GatewayTarget {
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
    // Version 1 stored reply data here. Preserve the bytes so existing records
    // remain readable while reply state becomes runtime-only.
    reserved_len: u16,
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
            reserved_len: 0,
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
        if self.version != RECORD_VERSION {
            return Err(BridgeError::InvalidRequest);
        }
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

pub(crate) async fn persist_mapping<Storage: PluginStorage>(
    storage: &Storage,
    mapping: &Mapping,
) -> Result<(), BridgeError> {
    let record = PersistedRoute::from_mapping(mapping)?;
    storage
        .put(&mapping.session, &record)
        .await
        .map_err(|_error| BridgeError::Storage)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::string::String;

    use embassy_time::{Duration, Instant};

    use super::{
        BridgeBook, GatewayEvent, PersistedRoute, RESERVATION_TIMEOUT, Resolution, ResolveResult,
        Route,
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
}

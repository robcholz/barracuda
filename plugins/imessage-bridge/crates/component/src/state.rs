use alloc::{
    collections::{BTreeMap, VecDeque},
    string::String,
};
use core::mem::size_of;

use barracuda_plugin::manager::PluginStorage;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

pub(crate) const CHANNEL_MAX: usize = 32;
pub(crate) const CONVERSATION_MAX: usize = 96;
pub(crate) const THREAD_MAX: usize = 64;
pub(crate) const MESSAGE_ID_MAX: usize = 96;
pub(crate) const SESSION_MAX: usize = 32;
const RECORD_VERSION: u32 = 1;

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolveResult {
    Missing,
    Found {
        session: String,
        open_required: bool,
    },
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

pub(crate) struct BridgeBook {
    by_route: BTreeMap<Route, String>,
    by_session: BTreeMap<String, Mapping>,
}

impl BridgeBook {
    pub(crate) const fn new() -> Self {
        Self {
            by_route: BTreeMap::new(),
            by_session: BTreeMap::new(),
        }
    }

    pub(crate) fn restore(
        &mut self,
        session: &str,
        persisted: PersistedRoute,
    ) -> Result<(), BridgeError> {
        if !valid_session(session) || self.by_session.contains_key(session) {
            return Err(BridgeError::InvalidRequest);
        }
        let mut mapping = persisted.into_mapping(session)?;
        mapping.opened = false;
        if self.by_route.contains_key(&mapping.route) {
            return Err(BridgeError::Conflict);
        }
        self.by_route
            .insert(mapping.route.clone(), mapping.session.clone());
        self.by_session.insert(mapping.session.clone(), mapping);
        Ok(())
    }

    pub(crate) fn resolve(&self, route: &Route) -> ResolveResult {
        let Some(session) = self.by_route.get(route) else {
            return ResolveResult::Missing;
        };
        let Some(mapping) = self.by_session.get(session) else {
            return ResolveResult::Missing;
        };
        ResolveResult::Found {
            session: mapping.session.clone(),
            open_required: !mapping.opened,
        }
    }

    pub(crate) fn commit_mapping(&mut self, mapping: Mapping) {
        self.by_route
            .insert(mapping.route.clone(), mapping.session.clone());
        self.by_session.insert(mapping.session.clone(), mapping);
    }

    pub(crate) fn prepare_binding(
        &self,
        route: Route,
        message_id: &str,
        session: &str,
    ) -> Result<Mapping, BridgeError> {
        if !valid_text(message_id, MESSAGE_ID_MAX) || !valid_session(session) {
            return Err(BridgeError::InvalidRequest);
        }
        if self
            .by_route
            .get(&route)
            .is_some_and(|existing| existing != session)
            || self
                .by_session
                .get(session)
                .is_some_and(|existing| existing.route != route)
        {
            return Err(BridgeError::Conflict);
        }
        let mut mapping = self.by_session.get(session).cloned().unwrap_or(Mapping {
            session: String::from(session),
            route,
            opened: true,
            pending_replies: VecDeque::new(),
            active_turn: None,
        });
        mapping.opened = true;
        mapping.pending_replies.push_back(String::from(message_id));
        Ok(mapping)
    }

    pub(crate) fn gateway_target(
        &mut self,
        session: &str,
        event: GatewayEvent,
    ) -> Result<Option<GatewayTarget>, BridgeError> {
        if !valid_session(session) {
            return Err(BridgeError::InvalidRequest);
        }
        let Some(mapping) = self.by_session.get_mut(session) else {
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
                });
            }
            GatewayEvent::OtherTurnStarted => {
                if mapping.active_turn.is_some() {
                    return Err(BridgeError::Conflict);
                }
                mapping.active_turn = Some(ActiveTurn { reply_to: None });
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

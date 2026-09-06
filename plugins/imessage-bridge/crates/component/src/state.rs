use alloc::{collections::BTreeMap, format, string::String};
use core::mem::size_of;

use barracuda_plugin_manager::PluginStorage;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

pub(crate) const CHANNEL_MAX: usize = 32;
pub(crate) const CONVERSATION_MAX: usize = 96;
pub(crate) const THREAD_MAX: usize = 64;
pub(crate) const MESSAGE_ID_MAX: usize = 96;
pub(crate) const SESSION_MAX: usize = 32;
const COMMAND_CAPACITY: usize = 16;
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
    reply_to: String,
    opened: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolveResult {
    Missing,
    Found {
        session: String,
        open_required: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BridgeError {
    InvalidRequest,
    Conflict,
    Storage,
    Busy,
    UnknownCommand,
}

impl BridgeError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Conflict => "conflict",
            Self::Storage => "storage",
            Self::Busy => "busy",
            Self::UnknownCommand => "unknown_command",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum GatewayCommand {
    Start {
        stream_id: String,
        route: Route,
        reply_to: String,
    },
    Chunk {
        stream_id: String,
        sequence: u32,
        boundary: &'static str,
        text: String,
    },
    Finish {
        stream_id: String,
        sequence: u32,
    },
}

#[derive(Clone, Debug)]
struct ActiveStream {
    stream_id: String,
    next_sequence: u32,
}

#[derive(Clone, Debug, Default)]
struct SessionEventState {
    logical_sequence: Option<u64>,
    event_type: String,
    active_stream: Option<ActiveStream>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionField<'a> {
    pub(crate) session: &'a str,
    pub(crate) run: &'a str,
    pub(crate) sequence: u64,
    pub(crate) field: &'a str,
    pub(crate) chunk: &'a str,
    pub(crate) field_complete: bool,
    pub(crate) terminal: Option<&'a str>,
}

pub(crate) struct BridgeBook {
    by_route: BTreeMap<Route, String>,
    by_session: BTreeMap<String, Mapping>,
    events: BTreeMap<String, SessionEventState>,
    commands: BTreeMap<String, GatewayCommand>,
    next_command: u64,
}

impl BridgeBook {
    pub(crate) const fn new() -> Self {
        Self {
            by_route: BTreeMap::new(),
            by_session: BTreeMap::new(),
            events: BTreeMap::new(),
            commands: BTreeMap::new(),
            next_command: 1,
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

    pub(crate) fn mapping_with_reply(
        &self,
        route: &Route,
        message_id: &str,
    ) -> Result<Mapping, BridgeError> {
        if !valid_text(message_id, MESSAGE_ID_MAX) {
            return Err(BridgeError::InvalidRequest);
        }
        let session = self
            .by_route
            .get(route)
            .ok_or(BridgeError::InvalidRequest)?;
        let mut mapping = self
            .by_session
            .get(session)
            .cloned()
            .ok_or(BridgeError::InvalidRequest)?;
        mapping.reply_to = String::from(message_id);
        Ok(mapping)
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
        Ok(Mapping {
            session: String::from(session),
            route,
            reply_to: String::from(message_id),
            opened: true,
        })
    }

    pub(crate) fn process_field(
        &mut self,
        field: SessionField<'_>,
    ) -> Result<Option<String>, BridgeError> {
        if !valid_session(field.session) || !valid_run(field.run) {
            return Err(BridgeError::InvalidRequest);
        }
        if field.terminal.is_some()
            && let Some(mapping) = self.by_session.get_mut(field.session)
        {
            mapping.opened = false;
        }
        if !self.by_session.contains_key(field.session) {
            return Ok(None);
        }

        let state = self.events.entry(String::from(field.session)).or_default();
        if state.logical_sequence != Some(field.sequence) {
            state.logical_sequence = Some(field.sequence);
            state.event_type.clear();
        }

        let mut command = if field.field == "type" {
            state.event_type.push_str(field.chunk);
            if !field.field_complete {
                None
            } else if state.event_type == "turn_started" && state.active_stream.is_none() {
                let mapping = self
                    .by_session
                    .get(field.session)
                    .cloned()
                    .ok_or(BridgeError::InvalidRequest)?;
                let stream_id = format!("{}.{}.{}", field.session, field.run, field.sequence);
                state.active_stream = Some(ActiveStream {
                    stream_id: stream_id.clone(),
                    next_sequence: 1,
                });
                Some(GatewayCommand::Start {
                    stream_id,
                    route: mapping.route,
                    reply_to: mapping.reply_to,
                })
            } else if state.event_type == "turn_ended" {
                state
                    .active_stream
                    .take()
                    .map(|active| GatewayCommand::Finish {
                        stream_id: active.stream_id,
                        sequence: active.next_sequence,
                    })
            } else {
                None
            }
        } else if field.field == "text" && state.event_type == "output_delta" {
            state.active_stream.as_mut().map(|active| {
                let sequence = active.next_sequence;
                active.next_sequence = active.next_sequence.saturating_add(1);
                GatewayCommand::Chunk {
                    stream_id: active.stream_id.clone(),
                    sequence,
                    boundary: if field.field_complete {
                        "complete"
                    } else {
                        "more"
                    },
                    text: String::from(field.chunk),
                }
            })
        } else {
            None
        };

        if command.is_none() && field.terminal.is_some() {
            command = state
                .active_stream
                .take()
                .map(|active| GatewayCommand::Finish {
                    stream_id: active.stream_id,
                    sequence: active.next_sequence,
                });
        }

        command.map(|value| self.insert_command(value)).transpose()
    }

    pub(crate) fn take_command(&mut self, id: &str) -> Result<GatewayCommand, BridgeError> {
        self.commands.remove(id).ok_or(BridgeError::UnknownCommand)
    }

    fn insert_command(&mut self, command: GatewayCommand) -> Result<String, BridgeError> {
        if self.commands.len() >= COMMAND_CAPACITY {
            return Err(BridgeError::Busy);
        }
        let id = format!("command-{}", self.next_command);
        self.next_command = self.next_command.checked_add(1).ok_or(BridgeError::Busy)?;
        self.commands.insert(id.clone(), command);
        Ok(id)
    }
}

fn valid_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max
}

fn valid_session(value: &str) -> bool {
    value.len() <= SESSION_MAX
        && value.strip_prefix("session-").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn valid_run(value: &str) -> bool {
    value.strip_prefix("run-").is_some_and(|suffix| {
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
    message_id_len: u16,
    channel: [u8; CHANNEL_MAX],
    conversation_id: [u8; CONVERSATION_MAX],
    thread_id: [u8; THREAD_MAX],
    message_id: [u8; MESSAGE_ID_MAX],
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
            message_id_len: u16::try_from(mapping.reply_to.len())
                .map_err(|_error| BridgeError::InvalidRequest)?,
            channel: [0; CHANNEL_MAX],
            conversation_id: [0; CONVERSATION_MAX],
            thread_id: [0; THREAD_MAX],
            message_id: [0; MESSAGE_ID_MAX],
        };
        copy_text(&mapping.route.channel, &mut record.channel)?;
        copy_text(&mapping.route.conversation_id, &mut record.conversation_id)?;
        if let Some(thread_id) = &mapping.route.thread_id {
            copy_text(thread_id, &mut record.thread_id)?;
        }
        copy_text(&mapping.reply_to, &mut record.message_id)?;
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
        let reply_to = read_text(&self.message_id, self.message_id_len)?;
        let route = Route::new(&channel, &conversation_id, thread_id.as_deref())?;
        Ok(Mapping {
            session: String::from(session),
            route,
            reply_to,
            opened: false,
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

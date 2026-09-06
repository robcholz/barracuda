//! JSON RPCs backed by an open `SessionControl` and bounded session Events.

use alloc::{collections::BTreeMap, rc::Rc};
use core::cell::RefCell;
use core::fmt::{self, Write as _};
use core::pin::Pin;
use core::task::{Context, Poll};

use async_channel::{Receiver, Sender};
use barracuda_agent_runtime::{
    ProviderUsage, SessionControl, SessionEvent, SessionId, SessionStream,
};
use barracuda_event_router::{
    ComponentError, ComponentResult, Event, EventEmitter, JsonPayload, RpcClient, RpcError,
};
use futures_lite::{future, future::poll_fn, Stream};

use crate::open_session::SessionEventDocument;

/// `SessionControl::append` JSON RPC.
pub mod append;
/// `SessionControl::cancel` JSON RPC.
pub mod cancel;
/// `SessionControl::close` JSON RPC.
pub mod close;
/// `SessionControl::interrupt` JSON RPC.
pub mod interrupt;
/// `SessionControl::respond` JSON RPC.
pub mod respond;
/// `SessionControl::set_permission_level` JSON RPC.
pub mod set_permission_level;
/// `SessionControl::set_reasoning_effort` JSON RPC.
pub mod set_reasoning_effort;

/// Bounded application-level event emitted for every open Agent session.
pub struct SessionOutputEvent;

impl Event for SessionOutputEvent {
    const ID: &'static str = "session.event";
}

const EVENT_DOCUMENT_FIXED_BYTES: usize =
    "{\"event\":\"".len() + SessionOutputEvent::ID.len() + "\",\"input\":".len() + "}".len();

struct OpenedSession {
    control: SessionControl,
    events: SessionStream,
}

struct RegistryState {
    next_run: u32,
    sessions: BTreeMap<SessionId, OpenedSession>,
}

struct RegistryInner {
    state: RefCell<RegistryState>,
    changed: Sender<()>,
    changes: Receiver<()>,
}

/// Shared controls and event streams established by `session.open`.
#[derive(Clone)]
pub struct SessionRegistry(Rc<RegistryInner>);

impl Default for SessionRegistry {
    fn default() -> Self {
        let (changed, changes) = async_channel::bounded(1);
        Self(Rc::new(RegistryInner {
            state: RefCell::new(RegistryState {
                next_run: 1,
                sessions: BTreeMap::new(),
            }),
            changed,
            changes,
        }))
    }
}

impl SessionRegistry {
    /// Returns a clone of the control for `session`.
    #[must_use]
    pub fn get(&self, session: SessionId) -> Option<SessionControl> {
        self.0
            .state
            .borrow()
            .sessions
            .get(&session)
            .map(|opened| opened.control.clone())
    }

    /// Installs one control/event pair and returns its correlation run number.
    pub fn insert(
        &self,
        session: SessionId,
        control: SessionControl,
        events: SessionStream,
    ) -> u32 {
        let mut state = self.0.state.borrow_mut();
        let run = state.next_run;
        state.next_run = state.next_run.checked_add(1).unwrap_or(1);
        state
            .sessions
            .insert(session, OpenedSession { control, events });
        drop(state);
        let _ignored = self.0.changed.try_send(());
        run
    }

    /// Removes and returns a session control.
    pub fn remove(&self, session: SessionId) -> Option<SessionControl> {
        self.0
            .state
            .borrow_mut()
            .sessions
            .remove(&session)
            .map(|opened| opened.control)
    }

    /// Removes every cached control and event stream.
    pub fn clear(&self) {
        self.0.state.borrow_mut().sessions.clear();
    }

    async fn next_event(&self) -> PendingEvent {
        loop {
            let event = poll_fn(|context| self.poll_event(context));
            let changed = async {
                let _ignored = self.0.changes.recv().await;
                None
            };
            if let Some(event) = future::or(event, changed).await {
                return event;
            }
        }
    }

    fn poll_event(&self, context: &mut Context<'_>) -> Poll<Option<PendingEvent>> {
        let mut state = self.0.state.borrow_mut();
        let mut ready = None;
        for (session, opened) in &mut state.sessions {
            match Stream::poll_next(Pin::new(&mut opened.events), context) {
                Poll::Ready(Some(Ok(event))) => {
                    let terminal = matches!(&event, SessionEvent::Closed(_));
                    ready = Some((*session, event.into(), terminal));
                    break;
                }
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => {
                    ready = Some((
                        *session,
                        SessionEventDocument::StreamError {
                            error: "worker_stopped",
                        },
                        true,
                    ));
                    break;
                }
                Poll::Pending => {}
            }
        }

        let Some((session, event, terminal)) = ready else {
            return Poll::Pending;
        };
        if terminal {
            state.sessions.remove(&session);
        }
        Poll::Ready(Some(PendingEvent { session, event }))
    }
}

struct PendingEvent {
    session: SessionId,
    event: SessionEventDocument,
}

/// Drives open runtime streams into bounded `session.event` documents.
pub(crate) async fn emit_session_events<const M: usize>(
    registry: SessionRegistry,
    rpc: RpcClient,
) -> ComponentResult<()> {
    let emitter = EventEmitter::<M>::new(rpc);
    let mut sequence = 0;
    loop {
        let pending = registry.next_event().await;
        sequence = SemanticEmitter::new(&emitter, pending.session, sequence)
            .emit_event(pending.event)
            .await?;
    }
}

struct SemanticEmitter<'a, const M: usize> {
    emitter: &'a EventEmitter<M>,
    session: SessionId,
    sequence: u32,
}

impl<'a, const M: usize> SemanticEmitter<'a, M> {
    const fn new(emitter: &'a EventEmitter<M>, session: SessionId, sequence: u32) -> Self {
        Self {
            emitter,
            session,
            sequence,
        }
    }

    async fn emit_event(mut self, event: SessionEventDocument) -> ComponentResult<u32> {
        use barracuda_agent_runtime::{InputRequestKind, TurnOrigin};
        use SessionEventDocument as Document;

        match event {
            Document::TurnStarted { turn, origin } => {
                let turn = IdText::new(turn)?;
                match origin {
                    TurnOrigin::User => {
                        self.emit(
                            "turn_started",
                            EventPayload::TurnStarted {
                                turn: turn.as_str(),
                                origin: "user",
                            },
                        )
                        .await?;
                    }
                    TurnOrigin::ToolCall { call } => {
                        self.emit(
                            "turn_started",
                            EventPayload::TurnStarted {
                                turn: turn.as_str(),
                                origin: "tool_call",
                            },
                        )
                        .await?;
                        self.text_events("turn_origin_tool_call_id_delta", &call.id)
                            .await?;
                        self.text_events("turn_origin_tool_name_delta", &call.name)
                            .await?;
                        self.text_events("turn_origin_arguments_delta", &call.arguments_json)
                            .await?;
                        self.emit("turn_origin_ended", EventPayload::Empty).await?;
                    }
                }
            }
            Document::InputRequested { request, kind } => {
                let request = IdText::new(request)?;
                match kind {
                    InputRequestKind::PermissionApproval { tool_call, reason } => {
                        self.emit(
                            "input_request_started",
                            EventPayload::InputRequestStarted {
                                request: request.as_str(),
                                kind: "permission_approval",
                            },
                        )
                        .await?;
                        self.text_events("input_request_tool_call_id_delta", &tool_call.id)
                            .await?;
                        self.text_events("input_request_tool_name_delta", &tool_call.name)
                            .await?;
                        self.text_events(
                            "input_request_arguments_delta",
                            &tool_call.arguments_json,
                        )
                        .await?;
                        self.text_events("input_request_reason_delta", &reason)
                            .await?;
                        self.emit(
                            "input_requested",
                            EventPayload::Request {
                                request: request.as_str(),
                            },
                        )
                        .await?;
                    }
                }
            }
            Document::IterationStarted { iteration } => {
                let iteration = IdText::new(iteration)?;
                self.emit(
                    "iteration_started",
                    EventPayload::Iteration {
                        iteration: iteration.as_str(),
                    },
                )
                .await?;
            }
            Document::ReasoningDelta { text } => {
                self.text_events("reasoning_delta", &text).await?;
            }
            Document::ReasoningEnded => {
                self.emit("reasoning_ended", EventPayload::Empty).await?;
            }
            Document::OutputDelta { text } => {
                self.text_events("output_delta", &text).await?;
            }
            Document::OutputEnded => {
                self.emit("output_ended", EventPayload::Empty).await?;
            }
            Document::ToolResult { call, output } => {
                self.emit("tool_result_started", EventPayload::Empty)
                    .await?;
                self.text_events("tool_call_id_delta", &call.id).await?;
                self.text_events("tool_name_delta", &call.name).await?;
                self.text_events("tool_arguments_delta", &call.arguments_json)
                    .await?;
                self.text_events("tool_output_delta", &output.content)
                    .await?;
                self.emit(
                    "tool_result_ended",
                    EventPayload::ToolResultEnded { ok: output.ok },
                )
                .await?;
            }
            Document::ToolResultsEnded => {
                self.emit("tool_results_ended", EventPayload::Empty).await?;
            }
            Document::IterationEnded => {
                self.emit("iteration_ended", EventPayload::Empty).await?;
            }
            Document::Usage { usage } => {
                self.emit("usage", EventPayload::Usage(usage)).await?;
            }
            Document::EffectOutputDelta { text } => {
                self.text_events("effect_output_delta", &text).await?;
            }
            Document::EffectOutputEnded => {
                self.emit("effect_output_ended", EventPayload::Empty)
                    .await?;
            }
            Document::TurnError { message } => {
                self.emit(
                    "turn_error",
                    EventPayload::Error {
                        message: message.as_str(),
                        truncated: message.truncated(),
                    },
                )
                .await?;
            }
            Document::TurnEnded { turn } => {
                let turn = IdText::new(turn)?;
                self.emit(
                    "turn_ended",
                    EventPayload::Turn {
                        turn: turn.as_str(),
                    },
                )
                .await?;
            }
            Document::SessionError { message } => {
                self.emit(
                    "session_error",
                    EventPayload::Error {
                        message: message.as_str(),
                        truncated: message.truncated(),
                    },
                )
                .await?;
            }
            Document::Closed { reason } => {
                self.emit(
                    "closed",
                    EventPayload::Reason {
                        reason: reason.code(),
                    },
                )
                .await?;
            }
            Document::StreamError { error } => {
                self.emit("stream_error", EventPayload::StreamError { error })
                    .await?;
            }
        }
        Ok(self.sequence)
    }

    async fn text_events(&mut self, event_type: &str, mut value: &str) -> ComponentResult<()> {
        loop {
            let final_payload = SessionEventPayload {
                session: self.session,
                sequence: self.sequence,
                event_type,
                payload: EventPayload::Text { text: value },
            };
            if emitted_document_len(&final_payload)? <= M {
                return self.emit_payload(&final_payload).await;
            }
            if value.is_empty() {
                return Err(ComponentError::lifecycle(EventLaneTooSmall));
            }

            let empty_payload = SessionEventPayload {
                session: self.session,
                sequence: self.sequence,
                event_type,
                payload: EventPayload::Text { text: "" },
            };
            let overhead = emitted_document_len(&empty_payload)?;
            let budget = M
                .checked_sub(overhead)
                .ok_or_else(|| ComponentError::lifecycle(EventLaneTooSmall))?;
            let chunk = json_bounded_prefix(value, budget);
            if chunk.is_empty() {
                return Err(ComponentError::lifecycle(EventLaneTooSmall));
            }
            let payload = SessionEventPayload {
                payload: EventPayload::Text { text: chunk },
                ..empty_payload
            };
            self.emit_payload(&payload).await?;
            value = value
                .get(chunk.len()..)
                .ok_or_else(|| ComponentError::lifecycle(EventEncodingFailed))?;
        }
    }

    async fn emit(&mut self, event_type: &str, payload: EventPayload<'_>) -> ComponentResult<()> {
        let payload = SessionEventPayload {
            session: self.session,
            sequence: self.sequence,
            event_type,
            payload,
        };
        if emitted_document_len(&payload)? > M {
            return Err(ComponentError::lifecycle(EventLaneTooSmall));
        }
        self.emit_payload(&payload).await
    }

    async fn emit_payload(&mut self, payload: &SessionEventPayload<'_>) -> ComponentResult<()> {
        self.emitter
            .emit::<SessionOutputEvent>(payload)
            .await
            .map_err(ComponentError::lifecycle)?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| ComponentError::lifecycle(EventSequenceOverflow))?;
        Ok(())
    }
}

struct SessionEventPayload<'a> {
    session: SessionId,
    sequence: u32,
    event_type: &'a str,
    payload: EventPayload<'a>,
}

enum EventPayload<'a> {
    Empty,
    Text {
        text: &'a str,
    },
    TurnStarted {
        turn: &'a str,
        origin: &'static str,
    },
    InputRequestStarted {
        request: &'a str,
        kind: &'static str,
    },
    Request {
        request: &'a str,
    },
    Iteration {
        iteration: &'a str,
    },
    ToolResultEnded {
        ok: bool,
    },
    Usage(ProviderUsage),
    Error {
        message: &'a str,
        truncated: bool,
    },
    Turn {
        turn: &'a str,
    },
    Reason {
        reason: &'a str,
    },
    StreamError {
        error: &'a str,
    },
}

impl JsonPayload for SessionEventPayload<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        let mut writer = CountingWriter(0);
        write_session_event(&mut writer, self).map_err(|_error| RpcError::InvalidFrameState)?;
        Ok(writer.0)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        let capacity = destination.len();
        let mut writer = SliceWriter {
            destination,
            written: 0,
        };
        write_session_event(&mut writer, self).map_err(|_error| RpcError::FrameTooLarge {
            size: capacity.saturating_add(1),
            capacity,
        })?;
        Ok(writer.written)
    }
}

fn write_session_event(
    writer: &mut dyn fmt::Write,
    event: &SessionEventPayload<'_>,
) -> fmt::Result {
    write!(
        writer,
        "{{\"session\":\"{}\",\"sequence\":{},\"type\":",
        event.session, event.sequence
    )?;
    write_json_string(writer, event.event_type)?;
    writer.write_str(",\"payload\":")?;
    write_event_payload(writer, &event.payload)?;
    writer.write_char('}')
}

fn write_event_payload(writer: &mut dyn fmt::Write, payload: &EventPayload<'_>) -> fmt::Result {
    writer.write_char('{')?;
    match payload {
        EventPayload::Empty => {}
        EventPayload::Text { text } => write_string_field(writer, "text", text)?,
        EventPayload::TurnStarted { turn, origin } => {
            write_string_field(writer, "turn", turn)?;
            writer.write_char(',')?;
            write_string_field(writer, "origin", origin)?;
        }
        EventPayload::InputRequestStarted { request, kind } => {
            write_string_field(writer, "request", request)?;
            writer.write_char(',')?;
            write_string_field(writer, "kind", kind)?;
        }
        EventPayload::Request { request } => write_string_field(writer, "request", request)?,
        EventPayload::Iteration { iteration } => {
            write_string_field(writer, "iteration", iteration)?;
        }
        EventPayload::ToolResultEnded { ok } => write!(writer, "\"ok\":{ok}")?,
        EventPayload::Usage(usage) => write_usage(writer, usage)?,
        EventPayload::Error { message, truncated } => {
            write_string_field(writer, "message", message)?;
            write!(writer, ",\"message_truncated\":{truncated}")?;
        }
        EventPayload::Turn { turn } => write_string_field(writer, "turn", turn)?,
        EventPayload::Reason { reason } => write_string_field(writer, "reason", reason)?,
        EventPayload::StreamError { error } => write_string_field(writer, "error", error)?,
    }
    writer.write_char('}')
}

fn write_string_field(writer: &mut dyn fmt::Write, name: &str, value: &str) -> fmt::Result {
    write_json_string(writer, name)?;
    writer.write_char(':')?;
    write_json_string(writer, value)
}

fn write_usage(writer: &mut dyn fmt::Write, usage: &ProviderUsage) -> fmt::Result {
    let values = [
        ("input_tokens", usage.input_tokens),
        ("output_tokens", usage.output_tokens),
        ("cache_read_tokens", usage.cache_read_tokens),
        ("cache_write_tokens", usage.cache_write_tokens),
    ];
    let mut written = false;
    for (name, value) in values {
        let Some(value) = value else {
            continue;
        };
        if written {
            writer.write_char(',')?;
        }
        write_json_string(writer, name)?;
        write!(writer, ":{value}")?;
        written = true;
    }
    Ok(())
}

fn emitted_document_len(payload: &SessionEventPayload<'_>) -> ComponentResult<usize> {
    payload
        .encoded_len()
        .and_then(|length| {
            length
                .checked_add(EVENT_DOCUMENT_FIXED_BYTES)
                .ok_or(RpcError::InvalidFrameState)
        })
        .map_err(ComponentError::lifecycle)
}

fn write_json_string(writer: &mut dyn fmt::Write, value: &str) -> fmt::Result {
    writer.write_char('"')?;
    for character in value.chars() {
        match character {
            '"' => writer.write_str("\\\"")?,
            '\\' => writer.write_str("\\\\")?,
            '\u{08}' => writer.write_str("\\b")?,
            '\u{0C}' => writer.write_str("\\f")?,
            '\n' => writer.write_str("\\n")?,
            '\r' => writer.write_str("\\r")?,
            '\t' => writer.write_str("\\t")?,
            character if character <= '\u{1F}' => {
                write!(writer, "\\u{:04x}", u32::from(character))?;
            }
            character => writer.write_char(character)?,
        }
    }
    writer.write_char('"')
}

fn json_bounded_prefix(value: &str, max_encoded_bytes: usize) -> &str {
    let mut encoded = 0_usize;
    let mut end = 0_usize;
    for (index, character) in value.char_indices() {
        let bytes = match character {
            '"' | '\\' => 2,
            '\u{08}' | '\u{0C}' | '\n' | '\r' | '\t' => 2,
            character if character <= '\u{1F}' => 6,
            character => character.len_utf8(),
        };
        let Some(next) = encoded.checked_add(bytes) else {
            break;
        };
        if next > max_encoded_bytes {
            break;
        }
        encoded = next;
        end = index.saturating_add(character.len_utf8());
    }
    value.get(..end).unwrap_or_default()
}

struct IdText {
    bytes: [u8; 24],
    len: usize,
}

impl IdText {
    fn new(value: impl fmt::Display) -> ComponentResult<Self> {
        let mut text = Self {
            bytes: [0; 24],
            len: 0,
        };
        write!(text, "{value}").map_err(|_error| ComponentError::lifecycle(EventEncodingFailed))?;
        Ok(text)
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(self.bytes.get(..self.len).unwrap_or(&[])).unwrap_or("")
    }
}

impl fmt::Write for IdText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(fmt::Error)?;
        let output = self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?;
        output.copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
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

#[derive(Debug)]
struct EventSequenceOverflow;

impl core::fmt::Display for EventSequenceOverflow {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("session event sequence overflow")
    }
}

impl core::error::Error for EventSequenceOverflow {}

#[derive(Debug)]
struct EventEncodingFailed;

impl core::fmt::Display for EventEncodingFailed {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("session event encoding failed")
    }
}

impl core::error::Error for EventEncodingFailed {}

#[derive(Debug)]
struct EventLaneTooSmall;

impl core::fmt::Display for EventLaneTooSmall {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Event lane cannot hold the session event envelope")
    }
}

impl core::error::Error for EventLaneTooSmall {}

#[cfg(test)]
mod tests {
    use alloc::format;

    use super::*;

    #[test]
    fn chunk_uses_the_available_event_lane_without_splitting_utf8() {
        let source = format!("{}😀{}", "a".repeat(400), "b".repeat(400));
        let empty = SessionEventPayload {
            session: SessionId::new(u32::MAX),
            sequence: u32::MAX,
            event_type: "output_delta",
            payload: EventPayload::Text { text: "" },
        };
        let budget = 512_usize.saturating_sub(emitted_document_len(&empty).unwrap_or(512));
        let chunk = json_bounded_prefix(&source, budget);

        assert!(chunk.len() > 32);
        assert!(chunk.len() < source.len());
        assert!(source.is_char_boundary(chunk.len()));
    }

    #[test]
    fn maximally_escaped_chunk_fits_one_event_lane() {
        let source = "\u{1f}".repeat(512);
        let empty = SessionEventPayload {
            session: SessionId::new(u32::MAX),
            sequence: u32::MAX,
            event_type: "tool_arguments_delta",
            payload: EventPayload::Text { text: "" },
        };
        let budget = 512_usize.saturating_sub(emitted_document_len(&empty).unwrap_or(512));
        let chunk = json_bounded_prefix(&source, budget);
        let payload = SessionEventPayload {
            payload: EventPayload::Text { text: chunk },
            ..empty
        };
        assert!(!chunk.is_empty());
        assert!(emitted_document_len(&payload).is_ok_and(|length| length <= 512));
    }
}

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

use crate::open_session::{SessionEventDocument, TerminalOutcome};

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
    run: u32,
    next_sequence: u32,
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
        state.sessions.insert(
            session,
            OpenedSession {
                control,
                events,
                run,
                next_sequence: 0,
            },
        );
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
                    let terminal = matches!(&event, SessionEvent::Closed(_))
                        .then_some(TerminalOutcome::Closed);
                    let sequence = opened.next_sequence;
                    opened.next_sequence = opened.next_sequence.saturating_add(1);
                    ready = Some((*session, opened.run, sequence, event.into(), terminal));
                    break;
                }
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => {
                    let sequence = opened.next_sequence;
                    ready = Some((
                        *session,
                        opened.run,
                        sequence,
                        SessionEventDocument::StreamError {
                            error: "worker_stopped",
                        },
                        Some(TerminalOutcome::WorkerStopped),
                    ));
                    break;
                }
                Poll::Pending => {}
            }
        }

        let Some((session, run, sequence, event, terminal)) = ready else {
            return Poll::Pending;
        };
        if terminal.is_some() {
            state.sessions.remove(&session);
        }
        Poll::Ready(Some(PendingEvent {
            session,
            run,
            sequence,
            event,
            terminal,
        }))
    }
}

struct PendingEvent {
    session: SessionId,
    run: u32,
    sequence: u32,
    event: SessionEventDocument,
    terminal: Option<TerminalOutcome>,
}

/// Drives open runtime streams into bounded `session.event` documents.
pub(crate) async fn emit_session_events<const M: usize>(
    registry: SessionRegistry,
    rpc: RpcClient,
) -> ComponentResult<()> {
    let emitter = EventEmitter::<M>::new(rpc);
    loop {
        let pending = registry.next_event().await;
        FieldEmitter::new(&emitter, &pending)
            .emit_event(pending.event)
            .await?;
    }
}

struct FieldEmitter<'a, const M: usize> {
    emitter: &'a EventEmitter<M>,
    session: SessionId,
    run: u32,
    sequence: u32,
    chunk_index: u32,
    terminal: Option<TerminalOutcome>,
}

impl<'a, const M: usize> FieldEmitter<'a, M> {
    const fn new(emitter: &'a EventEmitter<M>, event: &PendingEvent) -> Self {
        Self {
            emitter,
            session: event.session,
            run: event.run,
            sequence: event.sequence,
            chunk_index: 0,
            terminal: event.terminal,
        }
    }

    async fn emit_event(&mut self, event: SessionEventDocument) -> ComponentResult<()> {
        use barracuda_agent_runtime::{InputRequestKind, TurnOrigin};
        use SessionEventDocument as Document;

        match event {
            Document::TurnStarted { turn, origin } => {
                self.field("type", "turn_started", false).await?;
                let turn = IdText::new(turn)?;
                self.field("turn", turn.as_str(), false).await?;
                match origin {
                    TurnOrigin::User => self.field("origin", "user", true).await,
                    TurnOrigin::ToolCall { call } => {
                        self.field("origin", "tool_call", false).await?;
                        self.tool_call(call, true).await
                    }
                }
            }
            Document::InputRequested { request, kind } => {
                self.field("type", "input_requested", false).await?;
                let request = IdText::new(request)?;
                self.field("request", request.as_str(), false).await?;
                match kind {
                    InputRequestKind::PermissionApproval { tool_call, reason } => {
                        self.field("kind", "permission_approval", false).await?;
                        self.tool_call(tool_call, false).await?;
                        self.field("reason", &reason, true).await
                    }
                }
            }
            Document::IterationStarted { iteration } => {
                self.field("type", "iteration_started", false).await?;
                let iteration = IdText::new(iteration)?;
                self.field("iteration", iteration.as_str(), true).await
            }
            Document::ReasoningDelta { text } => {
                self.field("type", "reasoning_delta", false).await?;
                self.field("text", &text, true).await
            }
            Document::ReasoningEnded => self.field("type", "reasoning_ended", true).await,
            Document::OutputDelta { text } => {
                self.field("type", "output_delta", false).await?;
                self.field("text", &text, true).await
            }
            Document::OutputEnded => self.field("type", "output_ended", true).await,
            Document::ToolResult { call, output } => {
                self.field("type", "tool_result", false).await?;
                self.tool_call(call, false).await?;
                self.field("output", &output.content, false).await?;
                self.field("ok", if output.ok { "true" } else { "false" }, true)
                    .await
            }
            Document::ToolResultsEnded => self.field("type", "tool_results_ended", true).await,
            Document::IterationEnded => self.field("type", "iteration_ended", true).await,
            Document::Usage { usage } => self.usage(usage).await,
            Document::EffectOutputDelta { text } => {
                self.field("type", "effect_output_delta", false).await?;
                self.field("text", &text, true).await
            }
            Document::EffectOutputEnded => self.field("type", "effect_output_ended", true).await,
            Document::TurnError { message } => {
                self.field("type", "turn_error", false).await?;
                self.field("message", message.as_str(), false).await?;
                self.field(
                    "message_truncated",
                    if message.truncated() { "true" } else { "false" },
                    true,
                )
                .await
            }
            Document::TurnEnded { turn } => {
                self.field("type", "turn_ended", false).await?;
                let turn = IdText::new(turn)?;
                self.field("turn", turn.as_str(), true).await
            }
            Document::SessionError { message } => {
                self.field("type", "session_error", false).await?;
                self.field("message", message.as_str(), false).await?;
                self.field(
                    "message_truncated",
                    if message.truncated() { "true" } else { "false" },
                    true,
                )
                .await
            }
            Document::Closed { reason } => {
                self.field("type", "closed", false).await?;
                self.field("reason", reason.code(), true).await
            }
            Document::StreamError { error } => {
                self.field("type", "stream_error", false).await?;
                self.field("error", error, true).await
            }
        }
    }

    async fn tool_call(
        &mut self,
        call: barracuda_agent_runtime::ToolCall,
        last: bool,
    ) -> ComponentResult<()> {
        self.field("tool_call_id", &call.id, false).await?;
        self.field("tool_name", &call.name, false).await?;
        self.field("tool_arguments_json", &call.arguments_json, last)
            .await
    }

    async fn usage(&mut self, usage: ProviderUsage) -> ComponentResult<()> {
        let values = [
            ("input_tokens", usage.input_tokens),
            ("output_tokens", usage.output_tokens),
            ("cache_read_tokens", usage.cache_read_tokens),
            ("cache_write_tokens", usage.cache_write_tokens),
        ];
        let mut remaining = values.iter().filter(|(_, value)| value.is_some()).count();
        self.field("type", "usage", remaining == 0).await?;
        for (name, value) in values {
            let Some(value) = value else {
                continue;
            };
            remaining = remaining.saturating_sub(1);
            let value = IdText::new(value)?;
            self.field(name, value.as_str(), remaining == 0).await?;
        }
        Ok(())
    }

    async fn field(&mut self, name: &str, mut value: &str, last: bool) -> ComponentResult<()> {
        loop {
            let final_payload = EventChunkPayload {
                session: self.session,
                run: self.run,
                sequence: self.sequence,
                chunk_index: self.chunk_index,
                field: name,
                chunk: value,
                field_complete: true,
                event_complete: last,
                terminal: last.then_some(self.terminal).flatten(),
            };
            if emitted_document_len(&final_payload)? <= M {
                return self.emit_chunk(&final_payload).await;
            }
            if value.is_empty() {
                return Err(ComponentError::lifecycle(EventLaneTooSmall));
            }

            let empty_payload = EventChunkPayload {
                session: self.session,
                run: self.run,
                sequence: self.sequence,
                chunk_index: self.chunk_index,
                field: name,
                chunk: "",
                field_complete: false,
                event_complete: false,
                terminal: None,
            };
            let overhead = emitted_document_len(&empty_payload)?;
            let budget = M
                .checked_sub(overhead)
                .ok_or_else(|| ComponentError::lifecycle(EventLaneTooSmall))?;
            let chunk = json_bounded_prefix(value, budget);
            if chunk.is_empty() {
                return Err(ComponentError::lifecycle(EventLaneTooSmall));
            }
            let payload = EventChunkPayload {
                chunk,
                ..empty_payload
            };
            self.emit_chunk(&payload).await?;
            value = value
                .get(chunk.len()..)
                .ok_or_else(|| ComponentError::lifecycle(EventEncodingFailed))?;
        }
    }

    async fn emit_chunk(&mut self, payload: &EventChunkPayload<'_>) -> ComponentResult<()> {
        self.emitter
            .emit::<SessionOutputEvent>(payload)
            .await
            .map_err(ComponentError::lifecycle)?;
        self.chunk_index = self
            .chunk_index
            .checked_add(1)
            .ok_or_else(|| ComponentError::lifecycle(EventSequenceOverflow))?;
        Ok(())
    }
}

struct EventChunkPayload<'a> {
    session: SessionId,
    run: u32,
    sequence: u32,
    chunk_index: u32,
    field: &'a str,
    chunk: &'a str,
    field_complete: bool,
    event_complete: bool,
    terminal: Option<TerminalOutcome>,
}

impl JsonPayload for EventChunkPayload<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        let mut writer = CountingWriter(0);
        write_event_chunk(&mut writer, self).map_err(|_error| RpcError::InvalidFrameState)?;
        Ok(writer.0)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        let capacity = destination.len();
        let mut writer = SliceWriter {
            destination,
            written: 0,
        };
        write_event_chunk(&mut writer, self).map_err(|_error| RpcError::FrameTooLarge {
            size: capacity.saturating_add(1),
            capacity,
        })?;
        Ok(writer.written)
    }
}

fn write_event_chunk(writer: &mut dyn fmt::Write, event: &EventChunkPayload<'_>) -> fmt::Result {
    write!(
        writer,
        "{{\"session\":\"{}\",\"run\":\"run-{}\",\"sequence\":{},\"chunk_index\":{},\"field\":",
        event.session, event.run, event.sequence, event.chunk_index
    )?;
    write_json_string(writer, event.field)?;
    writer.write_str(",\"chunk\":")?;
    write_json_string(writer, event.chunk)?;
    write!(
        writer,
        ",\"field_complete\":{},\"event_complete\":{},\"terminal\":",
        event.field_complete, event.event_complete
    )?;
    match event.terminal {
        Some(terminal) => write_json_string(writer, terminal.code())?,
        None => writer.write_str("null")?,
    }
    writer.write_char('}')
}

fn emitted_document_len(payload: &EventChunkPayload<'_>) -> ComponentResult<usize> {
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
        formatter.write_str("session event chunk sequence overflow")
    }
}

impl core::error::Error for EventSequenceOverflow {}

#[derive(Debug)]
struct EventEncodingFailed;

impl core::fmt::Display for EventEncodingFailed {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("session event field encoding failed")
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
        let empty = EventChunkPayload {
            session: SessionId::new(u32::MAX),
            run: u32::MAX,
            sequence: u32::MAX,
            chunk_index: u32::MAX,
            field: "text",
            chunk: "",
            field_complete: false,
            event_complete: false,
            terminal: None,
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
        let empty = EventChunkPayload {
            session: SessionId::new(u32::MAX),
            run: u32::MAX,
            sequence: u32::MAX,
            chunk_index: u32::MAX,
            field: "tool_arguments_json",
            chunk: "",
            field_complete: false,
            event_complete: false,
            terminal: None,
        };
        let budget = 512_usize.saturating_sub(emitted_document_len(&empty).unwrap_or(512));
        let chunk = json_bounded_prefix(&source, budget);
        let payload = EventChunkPayload { chunk, ..empty };
        assert!(!chunk.is_empty());
        assert!(emitted_document_len(&payload).is_ok_and(|length| length <= 512));
    }
}

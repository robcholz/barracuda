//! Streaming SSE parsing: turn a provider's `text/event-stream` body into
//! ordered [`ChatStreamEvent`]s.
//!
//! Each parser is a **sync** state machine driven by raw byte chunks, kept free
//! of any async/transport concern so it can be unit-tested with byte slices
//! (including frames split mid-chunk and multibyte UTF-8 split across chunk
//! boundaries). The async [`crate::ChatStream`] wrapper only pumps bytes into
//! [`SseParse::push`] and reads semantic events back out.
//!
//! Ordering contract (both providers): within one response the three logical
//! streams are explicitly closed in order: `Reasoning(Delta)* ->
//! Reasoning(End) -> Output(Delta)* -> Output(End) -> ToolCalls(Delta)* ->
//! ToolCalls(End)`.

use claw_utils::stream::StreamPart;
use serde_json::Value;

use super::super::errors::{ChatError, ClawApiError};
use super::super::types::{ChatStreamEvent, ToolCall};

/// SSE event separators. Providers may use LF or HTTP-style CRLF lines.
const LF_FRAME_BOUNDARY: &[u8] = b"\n\n";
const CRLF_FRAME_BOUNDARY: &[u8] = b"\r\n\r\n";

/// The concrete SSE parser for the selected backend. Lets [`crate::ChatStream`]
/// stay one non-generic type while dispatching to the right provider parser.
pub(crate) enum ProviderSse {
    OpenAi(OpenAiSse),
    Anthropic(AnthropicSse),
}

impl ProviderSse {
    pub(crate) fn push(
        &mut self,
        bytes: &[u8],
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), ChatError> {
        match self {
            Self::OpenAi(parser) => parser.push(bytes, out),
            Self::Anthropic(parser) => parser.push(bytes, out),
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        match self {
            Self::OpenAi(parser) => parser.is_done(),
            Self::Anthropic(parser) => parser.is_done(),
        }
    }
}

/// A provider-specific streaming parser.
pub(crate) trait SseParse {
    /// Feed the next raw body chunk, appending newly-produced events to `out`.
    /// Partial frames are buffered until a later call completes them.
    fn push(&mut self, bytes: &[u8], out: &mut Vec<ChatStreamEvent>) -> Result<(), ChatError>;

    /// Whether the provider's native terminal event has been parsed.
    fn is_done(&self) -> bool;
}

/// Emits the provider-independent logical stream boundaries and rejects
/// provider events that move backwards across a completed content stream.
#[derive(Default)]
struct ContentEvents {
    phase: ContentPhase,
    emitted: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ContentPhase {
    #[default]
    Reasoning,
    Output,
    ToolCalls,
    Ended,
}

impl ContentEvents {
    fn reasoning(
        &mut self,
        fragment: String,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), ChatError> {
        if self.phase != ContentPhase::Reasoning {
            return Err(ClawApiError::Parse.into());
        }
        self.emitted = true;
        out.push(ChatStreamEvent::Reasoning(StreamPart::Delta(fragment)));
        Ok(())
    }

    fn output(
        &mut self,
        fragment: String,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), ChatError> {
        self.finish_reasoning(out);
        if self.phase != ContentPhase::Output {
            return Err(ClawApiError::Parse.into());
        }
        self.emitted = true;
        out.push(ChatStreamEvent::Output(StreamPart::Delta(fragment)));
        Ok(())
    }

    fn tool_call(
        &mut self,
        call: ToolCall,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), ChatError> {
        self.finish_reasoning(out);
        self.finish_output(out);
        if self.phase != ContentPhase::ToolCalls {
            return Err(ClawApiError::Parse.into());
        }
        self.emitted = true;
        out.push(ChatStreamEvent::ToolCalls(StreamPart::Delta(call)));
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<ChatStreamEvent>) -> Result<(), ChatError> {
        if self.phase == ContentPhase::Ended {
            return Err(ClawApiError::Parse.into());
        }
        self.finish_reasoning(out);
        self.finish_output(out);
        self.finish_tool_calls(out);
        Ok(())
    }

    fn finish_reasoning(&mut self, out: &mut Vec<ChatStreamEvent>) {
        if self.phase == ContentPhase::Reasoning {
            out.push(ChatStreamEvent::Reasoning(StreamPart::End));
            self.phase = ContentPhase::Output;
        }
    }

    fn finish_output(&mut self, out: &mut Vec<ChatStreamEvent>) {
        if self.phase == ContentPhase::Output {
            out.push(ChatStreamEvent::Output(StreamPart::End));
            self.phase = ContentPhase::ToolCalls;
        }
    }

    fn finish_tool_calls(&mut self, out: &mut Vec<ChatStreamEvent>) {
        if self.phase == ContentPhase::ToolCalls {
            out.push(ChatStreamEvent::ToolCalls(StreamPart::End));
            self.phase = ContentPhase::Ended;
        }
    }

    fn has_delta(&self) -> bool {
        self.emitted
    }
}

/// Buffers raw bytes and yields complete SSE frames (the text between blank
/// lines). Cutting only on ASCII line endings never splits a multibyte code
/// point, so a returned frame is always valid UTF-8.
#[derive(Default)]
struct FrameBuffer {
    buf: Vec<u8>,
}

impl FrameBuffer {
    fn push_bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn next_frame(&mut self) -> Result<Option<String>, ChatError> {
        let lf = find_subsequence(&self.buf, LF_FRAME_BOUNDARY)
            .map(|index| (index, LF_FRAME_BOUNDARY.len()));
        let crlf = find_subsequence(&self.buf, CRLF_FRAME_BOUNDARY)
            .map(|index| (index, CRLF_FRAME_BOUNDARY.len()));
        let boundary = match (lf, crlf) {
            (Some(lf), Some(crlf)) if lf.0 <= crlf.0 => Some(lf),
            (Some(_), Some(crlf)) => Some(crlf),
            (Some(lf), None) => Some(lf),
            (None, Some(crlf)) => Some(crlf),
            (None, None) => None,
        };
        let Some((idx, boundary_len)) = boundary else {
            return Ok(None);
        };
        let frame_end = idx.checked_add(boundary_len).ok_or(ClawApiError::Parse)?;
        let frame: Vec<u8> = self.buf.drain(..frame_end).collect();
        let payload = frame.get(..idx).ok_or(ClawApiError::Parse)?;
        let text = core::str::from_utf8(payload).map_err(|_| ClawApiError::Parse)?;
        Ok(Some(text.to_string()))
    }
}

/// Yield each `data:` payload in an SSE frame (skipping `event:`, comments, and
/// blank continuation lines).
fn data_payloads(frame: &str) -> impl Iterator<Item = &str> {
    frame.split('\n').filter_map(|line| {
        let payload = line.trim_end_matches('\r').strip_prefix("data:")?;
        let payload = payload.trim_start();
        (!payload.is_empty()).then_some(payload)
    })
}

// ---------------------------------------------------------------------------
// OpenAI-compatible
// ---------------------------------------------------------------------------

/// The OpenAI stream terminator payload.
const OPENAI_DONE: &str = "[DONE]";

/// Accumulated state for one streamed OpenAI tool call, keyed by its `index`.
#[derive(Default)]
struct OpenAiToolCall {
    id: String,
    name: String,
    args: String,
}

/// Incremental parser for an OpenAI-compatible `chat/completions` SSE stream.
#[derive(Default)]
pub(crate) struct OpenAiSse {
    frames: FrameBuffer,
    done: bool,
    events: ContentEvents,
    tool_calls: Vec<OpenAiToolCall>,
}

impl OpenAiSse {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn process_data(
        &mut self,
        payload: &str,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), ChatError> {
        let value: Value = serde_json::from_str(payload).map_err(|_| ClawApiError::Parse)?;
        let Some(delta) = value
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("delta"))
        else {
            return Ok(()); // e.g. a usage-only final chunk carries no delta
        };

        if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !reasoning.is_empty() {
                self.events.reasoning(reasoning.to_string(), out)?;
            }
        }
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                self.events.output(content.to_string(), out)?;
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                self.merge_tool_call(call)?;
            }
        }
        Ok(())
    }

    fn merge_tool_call(&mut self, call: &Value) -> Result<(), ChatError> {
        let raw_index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
        let index = usize::try_from(u32::try_from(raw_index).map_err(|_| ClawApiError::Parse)?)
            .map_err(|_| ClawApiError::Parse)?;
        if index > self.tool_calls.len() {
            return Err(ClawApiError::Parse.into());
        }
        if index == self.tool_calls.len() {
            self.tool_calls.push(OpenAiToolCall::default());
        }
        let slot = self.tool_calls.get_mut(index).ok_or(ClawApiError::Parse)?;
        if let Some(id) = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            slot.id = id.to_string();
        }
        if let Some(function) = call.get("function") {
            if let Some(name) = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                slot.name = name.to_string();
            }
            if let Some(args) = function.get("arguments").and_then(Value::as_str) {
                slot.args.push_str(args);
            }
        }
        Ok(())
    }

    /// Emit every complete call in index order at OpenAI's `[DONE]` marker.
    fn flush_tool_calls(&mut self, out: &mut Vec<ChatStreamEvent>) -> Result<(), ChatError> {
        for slot in self.tool_calls.drain(..) {
            if slot.name.is_empty() {
                continue;
            }
            self.events.tool_call(
                ToolCall {
                    id: slot.id,
                    name: slot.name,
                    arguments_json: slot.args,
                },
                out,
            )?;
        }
        Ok(())
    }
}

impl SseParse for OpenAiSse {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<ChatStreamEvent>) -> Result<(), ChatError> {
        self.frames.push_bytes(bytes);
        while let Some(frame) = self.frames.next_frame()? {
            for payload in data_payloads(&frame) {
                if payload == OPENAI_DONE {
                    self.flush_tool_calls(out)?;
                    if !self.events.has_delta() {
                        return Err(ClawApiError::EmptyResponse.into());
                    }
                    self.events.finish(out)?;
                    self.done = true;
                } else {
                    self.process_data(payload, out)?;
                }
                if self.done {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn is_done(&self) -> bool {
        self.done
    }
}

// ---------------------------------------------------------------------------
// Anthropic
// ---------------------------------------------------------------------------

/// One content block in an Anthropic assistant message, by block index.
enum AnthBlock {
    ToolUse {
        id: String,
        name: String,
        args: String,
    },
    Other,
}

/// Incremental parser for an Anthropic Messages API SSE stream.
#[derive(Default)]
pub(crate) struct AnthropicSse {
    frames: FrameBuffer,
    done: bool,
    events: ContentEvents,
    /// Content blocks by their Anthropic content-block index (contiguous).
    blocks: Vec<AnthBlock>,
}

impl AnthropicSse {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn process_data(
        &mut self,
        payload: &str,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), ChatError> {
        let value: Value = serde_json::from_str(payload).map_err(|_| ClawApiError::Parse)?;
        match value.get("type").and_then(Value::as_str) {
            Some("content_block_start") => self.on_block_start(&value)?,
            Some("content_block_delta") => self.on_block_delta(&value, out)?,
            Some("content_block_stop") => self.on_block_stop(&value, out)?,
            Some("message_stop") => {
                if !self.events.has_delta() {
                    return Err(ClawApiError::EmptyResponse.into());
                }
                self.events.finish(out)?;
                self.done = true;
            }
            _ => {} // message_start / message_delta / ping: ignored
        }
        Ok(())
    }

    fn slot(&mut self, index: usize) -> Result<&mut AnthBlock, ChatError> {
        if index > self.blocks.len() {
            return Err(ClawApiError::Parse.into());
        }
        if index == self.blocks.len() {
            self.blocks.push(AnthBlock::Other);
        }
        self.blocks
            .get_mut(index)
            .ok_or_else(|| ClawApiError::Parse.into())
    }

    fn on_block_start(&mut self, value: &Value) -> Result<(), ChatError> {
        let index = block_index(value)?;
        let kind = value
            .get("content_block")
            .and_then(|b| b.get("type"))
            .and_then(Value::as_str);
        let block = match kind {
            Some("tool_use") => {
                let content_block = value.get("content_block");
                AnthBlock::ToolUse {
                    id: content_block
                        .and_then(|b| b.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: content_block
                        .and_then(|b| b.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    args: String::new(),
                }
            }
            _ => AnthBlock::Other,
        };
        *self.slot(index)? = block;
        Ok(())
    }

    fn on_block_delta(
        &mut self,
        value: &Value,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), ChatError> {
        let index = block_index(value)?;
        let Some(delta) = value.get("delta") else {
            return Ok(());
        };
        match delta.get("type").and_then(Value::as_str) {
            Some("thinking_delta") => {
                if let Some(fragment) = delta.get("thinking").and_then(Value::as_str) {
                    if !fragment.is_empty() {
                        self.events.reasoning(fragment.to_string(), out)?;
                    }
                }
            }
            Some("signature_delta") => {}
            Some("text_delta") => {
                if let Some(fragment) = delta.get("text").and_then(Value::as_str) {
                    if !fragment.is_empty() {
                        self.events.output(fragment.to_string(), out)?;
                    }
                }
            }
            Some("input_json_delta") => {
                if let Some(fragment) = delta.get("partial_json").and_then(Value::as_str) {
                    if let AnthBlock::ToolUse { args, .. } = self.slot(index)? {
                        args.push_str(fragment);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn on_block_stop(
        &mut self,
        value: &Value,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), ChatError> {
        let index = block_index(value)?;
        let Some(block) = self.blocks.get_mut(index) else {
            return Ok(());
        };
        let AnthBlock::ToolUse { id, name, args } = std::mem::replace(block, AnthBlock::Other)
        else {
            return Ok(());
        };
        if !name.is_empty() {
            self.events.tool_call(
                ToolCall {
                    id,
                    name,
                    arguments_json: args,
                },
                out,
            )?;
        }
        Ok(())
    }
}

impl SseParse for AnthropicSse {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<ChatStreamEvent>) -> Result<(), ChatError> {
        self.frames.push_bytes(bytes);
        while let Some(frame) = self.frames.next_frame()? {
            for payload in data_payloads(&frame) {
                self.process_data(payload, out)?;
                if self.done {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn is_done(&self) -> bool {
        self.done
    }
}

fn block_index(value: &Value) -> Result<usize, ChatError> {
    let raw_index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
    usize::try_from(u32::try_from(raw_index).map_err(|_| ClawApiError::Parse)?)
        .map_err(|_| ClawApiError::Parse.into())
}

/// Index of the first occurrence of `needle` in `haystack`, if any.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive<P: SseParse>(parser: &mut P, body: &str) -> Vec<ChatStreamEvent> {
        let mut out = Vec::new();
        parser.push(body.as_bytes(), &mut out).unwrap();
        out
    }

    // ----- OpenAI -----

    #[test]
    fn openai_emits_explicit_content_stream_boundaries_in_order() {
        let mut parser = OpenAiSse::new();
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"foo\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"a\\\":1}\"}}]}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let deltas = drive(&mut parser, body);
        assert_eq!(
            deltas,
            vec![
                ChatStreamEvent::Reasoning(StreamPart::Delta("think".to_string())),
                ChatStreamEvent::Reasoning(StreamPart::End),
                ChatStreamEvent::Output(StreamPart::Delta("Hel".to_string())),
                ChatStreamEvent::Output(StreamPart::Delta("lo".to_string())),
                ChatStreamEvent::Output(StreamPart::End),
                ChatStreamEvent::ToolCalls(StreamPart::Delta(ToolCall {
                    id: "call_1".to_string(),
                    name: "foo".to_string(),
                    arguments_json: "{\"a\":1}".to_string(),
                })),
                ChatStreamEvent::ToolCalls(StreamPart::End),
            ]
        );
        assert!(parser.is_done());
    }

    #[test]
    fn openai_reassembles_frames_split_across_chunks() {
        let mut parser = OpenAiSse::new();
        let mut out = Vec::new();
        let full = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let (a, b) = full.split_at(10);
        let (b, c) = b.split_at(15);
        parser.push(a.as_bytes(), &mut out).unwrap();
        assert!(out.is_empty());
        parser.push(b.as_bytes(), &mut out).unwrap();
        assert!(out.is_empty());
        parser.push(c.as_bytes(), &mut out).unwrap();
        assert_eq!(
            out,
            vec![
                ChatStreamEvent::Reasoning(StreamPart::End),
                ChatStreamEvent::Output(StreamPart::Delta("hi".to_string())),
            ]
        );
    }

    #[test]
    fn openai_accepts_crlf_sse_frames() {
        let mut parser = OpenAiSse::new();
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\r\n\r\n",
            "data: [DONE]\r\n\r\n",
        );
        let deltas = drive(&mut parser, body);
        assert_eq!(
            deltas,
            vec![
                ChatStreamEvent::Reasoning(StreamPart::End),
                ChatStreamEvent::Output(StreamPart::Delta("hi".to_string())),
                ChatStreamEvent::Output(StreamPart::End),
                ChatStreamEvent::ToolCalls(StreamPart::End),
            ]
        );
        assert!(parser.is_done());
    }

    #[test]
    fn openai_reassembles_multibyte_utf8_split_across_chunks() {
        let mut parser = OpenAiSse::new();
        let mut out = Vec::new();
        let full = "data: {\"choices\":[{\"delta\":{\"content\":\"上\"}}]}\n\n";
        let bytes = full.as_bytes();
        let cut = full.find('上').unwrap() + 1;
        parser.push(&bytes[..cut], &mut out).unwrap();
        parser.push(&bytes[cut..], &mut out).unwrap();
        assert_eq!(
            out,
            vec![
                ChatStreamEvent::Reasoning(StreamPart::End),
                ChatStreamEvent::Output(StreamPart::Delta("上".to_string())),
            ]
        );
    }

    #[test]
    fn openai_empty_stream_is_an_error() {
        let mut parser = OpenAiSse::new();
        let mut out = Vec::new();
        assert!(parser.push(b"data: [DONE]\n\n", &mut out).is_err());
        assert!(out.is_empty());
        assert!(!parser.is_done());
    }

    #[test]
    fn openai_requires_done_marker() {
        let mut parser = OpenAiSse::new();
        drive(
            &mut parser,
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        );
        assert!(!parser.is_done());
    }

    // ----- Anthropic -----

    #[test]
    fn anthropic_emits_explicit_content_stream_boundaries_in_order() {
        let mut parser = AnthropicSse::new();
        let body = concat!(
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"foo\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"a\\\":\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"1}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let deltas = drive(&mut parser, body);
        assert_eq!(
            deltas,
            vec![
                ChatStreamEvent::Reasoning(StreamPart::Delta("hmm".to_string())),
                ChatStreamEvent::Reasoning(StreamPart::End),
                ChatStreamEvent::Output(StreamPart::Delta("Hi".to_string())),
                ChatStreamEvent::Output(StreamPart::End),
                ChatStreamEvent::ToolCalls(StreamPart::Delta(ToolCall {
                    id: "toolu_1".to_string(),
                    name: "foo".to_string(),
                    arguments_json: "{\"a\":1}".to_string(),
                })),
                ChatStreamEvent::ToolCalls(StreamPart::End),
            ]
        );
        assert!(parser.is_done());
    }

    #[test]
    fn anthropic_reassembles_frames_split_across_chunks() {
        let mut parser = AnthropicSse::new();
        let mut out = Vec::new();
        let full = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n";
        let (a, b) = full.split_at(40);
        parser.push(a.as_bytes(), &mut out).unwrap();
        assert!(out.is_empty());
        parser.push(b.as_bytes(), &mut out).unwrap();
        assert_eq!(
            out,
            vec![
                ChatStreamEvent::Reasoning(StreamPart::End),
                ChatStreamEvent::Output(StreamPart::Delta("hi".to_string())),
            ]
        );
    }

    #[test]
    fn anthropic_requires_message_stop() {
        let mut parser = AnthropicSse::new();
        drive(
            &mut parser,
            concat!(
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
            ),
        );
        assert!(!parser.is_done());
    }
}

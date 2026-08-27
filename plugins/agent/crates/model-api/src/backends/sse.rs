//! Streaming SSE parsing: turn a provider's `text/event-stream` body into
//! ordered [`ChatStreamEvent`]s.
//!
//! `eventsource-stream` owns standards-compliant SSE framing, including
//! fragmented UTF-8 and multiline `data:` fields. Each provider parser remains
//! a synchronous state machine driven by one complete SSE data payload at a
//! time.
//!
//! Ordering contract (both providers): within one response the three logical
//! streams are explicitly closed in order: `Reasoning(Delta)* ->
//! Reasoning(End) -> Output(Delta)* -> Output(End) -> ToolCalls(Delta)* ->
//! ToolCalls(End)`. With cache profiling enabled, one final `Usage` event may
//! follow those boundaries.

use alloc::string::String;
use alloc::vec::Vec;
use barracuda_runtime_utils::stream::StreamPart;
use serde::Deserialize;

use super::super::errors::Error;
#[cfg(feature = "cache_profile")]
use super::super::types::ProviderUsage;
use super::super::types::{ChatStreamEvent, ToolCall};
#[cfg(feature = "cache_profile")]
use super::shared::{AnthropicUsage, OpenAiUsage};

#[cfg(feature = "cache_profile")]
fn merge_usage(current: &mut Option<ProviderUsage>, incoming: ProviderUsage) {
    let aggregate = current.get_or_insert_default();
    aggregate.input_tokens = incoming.input_tokens.or(aggregate.input_tokens);
    aggregate.output_tokens = incoming.output_tokens.or(aggregate.output_tokens);
    aggregate.cache_read_tokens = incoming.cache_read_tokens.or(aggregate.cache_read_tokens);
    aggregate.cache_write_tokens = incoming.cache_write_tokens.or(aggregate.cache_write_tokens);
}

/// The concrete SSE parser for the selected backend. Lets [`crate::ChatStream`]
/// stay one non-generic type while dispatching to the right provider parser.
pub(crate) enum ProviderSse {
    OpenAi(OpenAiSse),
    Anthropic(AnthropicSse),
}

impl ProviderSse {
    pub(crate) fn process_data(
        &mut self,
        payload: &str,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), Error> {
        match self {
            Self::OpenAi(parser) => parser.process_data(payload, out),
            Self::Anthropic(parser) => parser.process_data(payload, out),
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        match self {
            Self::OpenAi(parser) => parser.done,
            Self::Anthropic(parser) => parser.done,
        }
    }
}

/// Emits the provider-independent logical stream boundaries and rejects
/// provider events that move backwards across a completed content stream.
#[derive(Default)]
struct ContentEvents {
    phase: ContentPhase,
    emitted: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum ContentPhase {
    #[default]
    Reasoning,
    Output,
    ToolCalls,
    Ended,
}

impl ContentEvents {
    fn reasoning(&mut self, fragment: String, out: &mut Vec<ChatStreamEvent>) -> Result<(), Error> {
        if self.phase != ContentPhase::Reasoning {
            return Err(Error::Parse);
        }
        self.emitted = true;
        out.push(ChatStreamEvent::Reasoning(StreamPart::Delta(fragment)));
        Ok(())
    }

    fn output(&mut self, fragment: String, out: &mut Vec<ChatStreamEvent>) -> Result<(), Error> {
        self.advance_to(ContentPhase::Output, out)?;
        self.emitted = true;
        out.push(ChatStreamEvent::Output(StreamPart::Delta(fragment)));
        Ok(())
    }

    fn tool_call(&mut self, call: ToolCall, out: &mut Vec<ChatStreamEvent>) -> Result<(), Error> {
        self.advance_to(ContentPhase::ToolCalls, out)?;
        self.emitted = true;
        out.push(ChatStreamEvent::ToolCalls(StreamPart::Delta(call)));
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<ChatStreamEvent>) -> Result<(), Error> {
        if self.phase == ContentPhase::Ended {
            return Err(Error::Parse);
        }
        self.advance_to(ContentPhase::Ended, out)
    }

    fn advance_to(
        &mut self,
        target: ContentPhase,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), Error> {
        if self.phase > target {
            return Err(Error::Parse);
        }
        while self.phase < target {
            let (event, next) = match self.phase {
                ContentPhase::Reasoning => (
                    ChatStreamEvent::Reasoning(StreamPart::End),
                    ContentPhase::Output,
                ),
                ContentPhase::Output => (
                    ChatStreamEvent::Output(StreamPart::End),
                    ContentPhase::ToolCalls,
                ),
                ContentPhase::ToolCalls => (
                    ChatStreamEvent::ToolCalls(StreamPart::End),
                    ContentPhase::Ended,
                ),
                ContentPhase::Ended => return Err(Error::Parse),
            };
            out.push(event);
            self.phase = next;
        }
        Ok(())
    }

    fn has_delta(&self) -> bool {
        self.emitted
    }
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

#[derive(Deserialize)]
struct OpenAiChunk {
    #[serde(default)]
    choices: Vec<OpenAiChunkChoice>,
    #[cfg(feature = "cache_profile")]
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiChunkChoice {
    delta: Option<OpenAiDelta>,
}

#[derive(Deserialize)]
struct OpenAiDelta {
    reasoning_content: Option<String>,
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCallDelta>,
}

#[derive(Deserialize)]
struct OpenAiToolCallDelta {
    #[serde(default)]
    index: u64,
    id: Option<String>,
    function: Option<OpenAiFunctionDelta>,
}

#[derive(Deserialize)]
struct OpenAiFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

/// Incremental parser for an OpenAI-compatible `chat/completions` SSE stream.
#[derive(Default)]
pub(crate) struct OpenAiSse {
    done: bool,
    events: ContentEvents,
    tool_calls: Vec<OpenAiToolCall>,
    #[cfg(feature = "cache_profile")]
    usage: Option<ProviderUsage>,
}

impl OpenAiSse {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn process_chunk(
        &mut self,
        payload: &str,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), Error> {
        let chunk: OpenAiChunk = serde_json::from_str(payload).map_err(|_| Error::Parse)?;
        #[cfg(feature = "cache_profile")]
        if let Some(usage) = chunk.usage.and_then(OpenAiUsage::profile) {
            merge_usage(&mut self.usage, usage);
        }
        let Some(delta) = chunk
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.delta)
        else {
            return Ok(()); // e.g. a usage-only final chunk carries no delta
        };

        if let Some(reasoning) = delta.reasoning_content {
            if !reasoning.is_empty() {
                self.events.reasoning(reasoning, out)?;
            }
        }
        if let Some(content) = delta.content {
            if !content.is_empty() {
                self.events.output(content, out)?;
            }
        }
        for call in delta.tool_calls {
            self.merge_tool_call(call)?;
        }
        Ok(())
    }

    fn merge_tool_call(&mut self, call: OpenAiToolCallDelta) -> Result<(), Error> {
        let index = usize::try_from(u32::try_from(call.index).map_err(|_| Error::Parse)?)
            .map_err(|_| Error::Parse)?;
        if index > self.tool_calls.len() {
            return Err(Error::Parse);
        }
        if index == self.tool_calls.len() {
            self.tool_calls.push(OpenAiToolCall::default());
        }
        let slot = self.tool_calls.get_mut(index).ok_or(Error::Parse)?;
        if let Some(id) = call.id.filter(|id| !id.is_empty()) {
            slot.id = id;
        }
        if let Some(function) = call.function {
            if let Some(name) = function.name.filter(|name| !name.is_empty()) {
                slot.name = name;
            }
            if let Some(arguments) = function.arguments {
                slot.args.push_str(&arguments);
            }
        }
        Ok(())
    }

    /// Emit every complete call in index order at OpenAI's `[DONE]` marker.
    fn flush_tool_calls(&mut self, out: &mut Vec<ChatStreamEvent>) -> Result<(), Error> {
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

    fn process_data(&mut self, payload: &str, out: &mut Vec<ChatStreamEvent>) -> Result<(), Error> {
        if payload == OPENAI_DONE {
            self.flush_tool_calls(out)?;
            if !self.events.has_delta() {
                return Err(Error::EmptyResponse);
            }
            self.events.finish(out)?;
            #[cfg(feature = "cache_profile")]
            if let Some(usage) = self.usage.take() {
                out.push(ChatStreamEvent::Usage(usage));
            }
            self.done = true;
        } else {
            self.process_chunk(payload, out)?;
        }
        Ok(())
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

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicEvent {
    MessageStart {
        message: Option<AnthropicMessageStart>,
    },
    MessageDelta {
        #[cfg(feature = "cache_profile")]
        usage: Option<AnthropicUsage>,
    },
    ContentBlockStart {
        #[serde(default)]
        index: u64,
        content_block: Option<AnthropicBlockStart>,
    },
    ContentBlockDelta {
        #[serde(default)]
        index: u64,
        delta: Option<AnthropicDelta>,
    },
    ContentBlockStop {
        #[serde(default)]
        index: u64,
    },
    MessageStop,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct AnthropicMessageStart {
    #[cfg(feature = "cache_profile")]
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicBlockStart {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicDelta {
    ThinkingDelta {
        thinking: Option<String>,
    },
    SignatureDelta,
    TextDelta {
        text: Option<String>,
    },
    InputJsonDelta {
        partial_json: Option<String>,
    },
    #[serde(other)]
    Other,
}

/// Incremental parser for an Anthropic Messages API SSE stream.
#[derive(Default)]
pub(crate) struct AnthropicSse {
    done: bool,
    events: ContentEvents,
    /// Content blocks by their Anthropic content-block index (contiguous).
    blocks: Vec<AnthBlock>,
    #[cfg(feature = "cache_profile")]
    usage: Option<ProviderUsage>,
}

impl AnthropicSse {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn process_data(&mut self, payload: &str, out: &mut Vec<ChatStreamEvent>) -> Result<(), Error> {
        let event: AnthropicEvent = serde_json::from_str(payload).map_err(|_| Error::Parse)?;
        match event {
            AnthropicEvent::MessageStart { message } => {
                #[cfg(not(feature = "cache_profile"))]
                let _ = message;
                #[cfg(feature = "cache_profile")]
                if let Some(usage) = message
                    .and_then(|message| message.usage)
                    .and_then(AnthropicUsage::profile)
                {
                    merge_usage(&mut self.usage, usage);
                }
            }
            AnthropicEvent::MessageDelta {
                #[cfg(feature = "cache_profile")]
                usage,
            } =>
            {
                #[cfg(feature = "cache_profile")]
                if let Some(usage) = usage.and_then(AnthropicUsage::profile) {
                    merge_usage(&mut self.usage, usage);
                }
            }
            AnthropicEvent::ContentBlockStart {
                index,
                content_block,
            } => self.on_block_start(index, content_block)?,
            AnthropicEvent::ContentBlockDelta { index, delta } => {
                self.on_block_delta(index, delta, out)?
            }
            AnthropicEvent::ContentBlockStop { index } => self.on_block_stop(index, out)?,
            AnthropicEvent::MessageStop => {
                if !self.events.has_delta() {
                    return Err(Error::EmptyResponse);
                }
                self.events.finish(out)?;
                #[cfg(feature = "cache_profile")]
                if let Some(usage) = self.usage.take() {
                    out.push(ChatStreamEvent::Usage(usage));
                }
                self.done = true;
            }
            AnthropicEvent::Other => {}
        }
        Ok(())
    }

    fn slot(&mut self, index: usize) -> Result<&mut AnthBlock, Error> {
        if index > self.blocks.len() {
            return Err(Error::Parse);
        }
        if index == self.blocks.len() {
            self.blocks.push(AnthBlock::Other);
        }
        self.blocks.get_mut(index).ok_or(Error::Parse)
    }

    fn on_block_start(
        &mut self,
        raw_index: u64,
        content_block: Option<AnthropicBlockStart>,
    ) -> Result<(), Error> {
        let index = block_index(raw_index)?;
        let block = match content_block {
            Some(content_block) if content_block.kind == "tool_use" => AnthBlock::ToolUse {
                id: content_block.id.unwrap_or_default(),
                name: content_block.name.unwrap_or_default(),
                args: String::new(),
            },
            _ => AnthBlock::Other,
        };
        *self.slot(index)? = block;
        Ok(())
    }

    fn on_block_delta(
        &mut self,
        raw_index: u64,
        delta: Option<AnthropicDelta>,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), Error> {
        let index = block_index(raw_index)?;
        let Some(delta) = delta else {
            return Ok(());
        };
        match delta {
            AnthropicDelta::ThinkingDelta { thinking } => {
                if let Some(fragment) = thinking {
                    if !fragment.is_empty() {
                        self.events.reasoning(fragment, out)?;
                    }
                }
            }
            AnthropicDelta::SignatureDelta => {}
            AnthropicDelta::TextDelta { text } => {
                if let Some(fragment) = text {
                    if !fragment.is_empty() {
                        self.events.output(fragment, out)?;
                    }
                }
            }
            AnthropicDelta::InputJsonDelta { partial_json } => {
                if let Some(fragment) = partial_json {
                    if let AnthBlock::ToolUse { args, .. } = self.slot(index)? {
                        args.push_str(&fragment);
                    }
                }
            }
            AnthropicDelta::Other => {}
        }
        Ok(())
    }

    fn on_block_stop(
        &mut self,
        raw_index: u64,
        out: &mut Vec<ChatStreamEvent>,
    ) -> Result<(), Error> {
        let index = block_index(raw_index)?;
        let Some(block) = self.blocks.get_mut(index) else {
            return Ok(());
        };
        let AnthBlock::ToolUse { id, name, args } = core::mem::replace(block, AnthBlock::Other)
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

fn block_index(raw_index: u64) -> Result<usize, Error> {
    usize::try_from(u32::try_from(raw_index).map_err(|_| Error::Parse)?).map_err(|_| Error::Parse)
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;
    use eventsource_stream::EventStream;
    use futures_lite::{future::block_on, StreamExt as _};

    use super::*;

    fn drive(parser: &mut ProviderSse, body: &str) -> Vec<ChatStreamEvent> {
        drive_chunks(parser, &[body.as_bytes()])
    }

    fn drive_chunks(parser: &mut ProviderSse, chunks: &[&[u8]]) -> Vec<ChatStreamEvent> {
        let mut out = Vec::new();
        let source = futures_lite::stream::iter(
            chunks.iter().map(|chunk| Ok::<Vec<u8>, ()>(chunk.to_vec())),
        );
        let mut events = EventStream::new(source);
        block_on(async {
            while let Some(event) = events.next().await {
                let event = event.expect("valid SSE event");
                parser
                    .process_data(&event.data, &mut out)
                    .expect("valid provider payload");
            }
        });
        out
    }

    // ----- OpenAI -----

    #[test]
    fn openai_emits_explicit_content_stream_boundaries_in_order() {
        let mut parser = ProviderSse::OpenAi(OpenAiSse::new());
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
        let mut parser = ProviderSse::OpenAi(OpenAiSse::new());
        let full = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let (a, b) = full.split_at(10);
        let (b, c) = b.split_at(15);
        let out = drive_chunks(&mut parser, &[a.as_bytes(), b.as_bytes(), c.as_bytes()]);
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
        let mut parser = ProviderSse::OpenAi(OpenAiSse::new());
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
    fn openai_joins_multiline_sse_data_before_parsing_json() {
        let mut parser = ProviderSse::OpenAi(OpenAiSse::new());
        let body = concat!(
            "data: {\"choices\":[{\"delta\":\n",
            "data: {\"content\":\"hi\"}}]}\n\n",
            "data: [DONE]\n\n",
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
        let mut parser = ProviderSse::OpenAi(OpenAiSse::new());
        let full = "data: {\"choices\":[{\"delta\":{\"content\":\"上\"}}]}\n\n";
        let bytes = full.as_bytes();
        let cut = full.find('上').unwrap() + 1;
        let out = drive_chunks(&mut parser, &[&bytes[..cut], &bytes[cut..]]);
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
        let mut parser = ProviderSse::OpenAi(OpenAiSse::new());
        let mut out = Vec::new();
        assert!(parser.process_data(OPENAI_DONE, &mut out).is_err());
        assert!(out.is_empty());
        assert!(!parser.is_done());
    }

    #[test]
    fn openai_requires_done_marker() {
        let mut parser = ProviderSse::OpenAi(OpenAiSse::new());
        drive(
            &mut parser,
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        );
        assert!(!parser.is_done());
    }

    #[cfg(feature = "cache_profile")]
    #[test]
    fn openai_emits_usage_once_after_content_boundaries() {
        let mut parser = ProviderSse::OpenAi(OpenAiSse::new());
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"prompt_tokens_details\":{\"cached_tokens\":8}}}\n\n",
            "data: [DONE]\n\n",
        );

        assert_eq!(
            drive(&mut parser, body),
            vec![
                ChatStreamEvent::Reasoning(StreamPart::End),
                ChatStreamEvent::Output(StreamPart::Delta("Hi".to_string())),
                ChatStreamEvent::Output(StreamPart::End),
                ChatStreamEvent::ToolCalls(StreamPart::End),
                ChatStreamEvent::Usage(ProviderUsage {
                    input_tokens: Some(12),
                    output_tokens: Some(3),
                    cache_read_tokens: Some(8),
                    cache_write_tokens: None,
                }),
            ]
        );
    }

    // ----- Anthropic -----

    #[test]
    fn anthropic_emits_explicit_content_stream_boundaries_in_order() {
        let mut parser = ProviderSse::Anthropic(AnthropicSse::new());
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
        let mut parser = ProviderSse::Anthropic(AnthropicSse::new());
        let full = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n";
        let (a, b) = full.split_at(40);
        let out = drive_chunks(&mut parser, &[a.as_bytes(), b.as_bytes()]);
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
        let mut parser = ProviderSse::Anthropic(AnthropicSse::new());
        drive(
            &mut parser,
            concat!(
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
            ),
        );
        assert!(!parser.is_done());
    }

    #[cfg(feature = "cache_profile")]
    #[test]
    fn anthropic_merges_usage_and_emits_it_once_after_content_boundaries() {
        let mut parser = ProviderSse::Anthropic(AnthropicSse::new());
        let body = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"cache_read_input_tokens\":12,\"cache_creation_input_tokens\":8}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );

        assert_eq!(
            drive(&mut parser, body),
            vec![
                ChatStreamEvent::Reasoning(StreamPart::End),
                ChatStreamEvent::Output(StreamPart::Delta("Hi".to_string())),
                ChatStreamEvent::Output(StreamPart::End),
                ChatStreamEvent::ToolCalls(StreamPart::End),
                ChatStreamEvent::Usage(ProviderUsage {
                    input_tokens: Some(20),
                    output_tokens: Some(5),
                    cache_read_tokens: Some(12),
                    cache_write_tokens: Some(8),
                }),
            ]
        );
    }
}

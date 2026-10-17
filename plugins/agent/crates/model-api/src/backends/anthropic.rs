//! Anthropic-compatible backend.
//!
//! Converts OpenAI-style messages/tools to the Anthropic Messages API shape and
//! parses the Anthropic content-block response back into a [`LlmResponse`].
//!
//! Structured JSON ([`crate::ModelApi::chat_json`]) uses Anthropic
//! `output_config.format` (this backend supports provider-native JSON schema).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use barracuda_bulk_memory::BulkVec;
use embedded_nal_async::{Dns, TcpConnect};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::super::attachments::{write_object_with_media, Inline, RequestMedia};
use super::super::chat_stream::ProviderStream;
use super::super::errors::Error;
use super::super::transport::Transport;
use super::super::types::{ChatRequest, LlmResponse, ToolCall};
use super::body::{encode, write_base64_str, Object};
#[cfg(feature = "cache_profile")]
use super::shared::AnthropicUsage;
use super::shared::{post_json, post_stream};
use super::sse::{AnthropicSse, ProviderSse};
use super::Backend;
use barracuda_agent_message::json::Sink;
use barracuda_agent_message::{Attachment, ChatMessage};

pub(super) const ANTHROPIC_VERSION: &str = "2023-06-01";
pub(super) const CHAT_PATH: &str = "/messages";

fn str_field<'a>(obj: &'a Value, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(|v| v.as_str())
}

fn make_tool_use_block(tool_call: &Value) -> Result<Value, Error> {
    if !tool_call.is_object() {
        return Err(Error::Api("invalid tool call in message history"));
    }
    let id = str_field(tool_call, "id")
        .filter(|id| !id.is_empty())
        .ok_or(Error::Api("invalid tool call in message history"))?;
    let function = tool_call.get("function");
    let name = function
        .and_then(|f| f.get("name"))
        .and_then(|n| n.as_str())
        .filter(|name| !name.is_empty())
        .ok_or(Error::Api("invalid tool call in message history"))?;
    let args = function
        .and_then(|f| f.get("arguments"))
        .and_then(|a| a.as_str());
    let input = match args {
        Some(s) if !s.is_empty() => serde_json::from_str::<Value>(s)
            .map_err(|_| Error::Api("invalid tool call arguments json"))?,
        _ => json!({}),
    };
    Ok(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
}

/// Converts the persisted `messages` history followed by the ephemeral
/// `reminders` (a two-segment tail) into the Anthropic message shape, viewed
/// as one sequence so consecutive-tool-message merging works across the seam.
///
/// A `media` part becomes a marker for the image or document the request
/// inlines, or a text block with its placeholder.
fn convert_messages_to_anthropic(
    messages: &[ChatMessage],
    reminders: &[ChatMessage],
    media: &RequestMedia,
) -> Result<Value, Error> {
    // The Anthropic shape regroups content blocks, so decode each message for
    // the duration of this request only.
    let decoded: Vec<Value> = messages
        .iter()
        .chain(reminders)
        .map(ChatMessage::to_value)
        .collect();
    let mut out: Vec<Value> = Vec::new();
    let mut iter = decoded.iter().enumerate().peekable();

    while let Some((index, msg)) = iter.next() {
        let role = match str_field(msg, "role") {
            Some(r) if !r.is_empty() => r,
            _ => continue,
        };

        // Merge consecutive "tool"-role messages into one "user" message.
        if role == "tool" {
            let mut tool_blocks: Vec<Value> = Vec::new();
            if let Some(block) = make_tool_result_block(msg, index, media) {
                tool_blocks.push(block);
            }
            while iter
                .peek()
                .is_some_and(|(_, next)| str_field(next, "role") == Some("tool"))
            {
                let Some((inner_index, inner)) = iter.next() else {
                    break;
                };
                if let Some(block) = make_tool_result_block(inner, inner_index, media) {
                    tool_blocks.push(block);
                }
            }
            if tool_blocks.is_empty() {
                continue;
            }
            out.push(json!({"role": "user", "content": tool_blocks}));
            continue;
        }

        if role != "assistant" && role != "user" {
            continue;
        }

        let mut blocks: Vec<Value> = Vec::new();
        let content = msg.get("content");
        match content {
            Some(Value::String(s)) if !s.is_empty() => {
                blocks.push(json!({"type": "text", "text": s}));
            }
            Some(Value::Array(items)) => {
                for (part, block) in items.iter().enumerate() {
                    if let Some(attachment) = Attachment::from_part(block) {
                        blocks.push(media.part(index, part, &attachment, |_inline| true));
                    } else if let Some(
                        "text" | "image" | "document" | "tool_use" | "tool_result" | "thinking"
                        | "redacted_thinking",
                    ) = str_field(block, "type")
                    {
                        blocks.push(block.clone());
                    }
                }
            }
            _ => {}
        }

        if role == "assistant" {
            // Anthropic verifies replayed thinking against its signature, so
            // reasoning without one (another provider's, or a response with
            // several thinking blocks) is left out rather than rejected.
            let has_thinking = blocks.iter().any(|block| {
                matches!(
                    str_field(block, "type"),
                    Some("thinking" | "redacted_thinking")
                )
            });
            let reasoning = str_field(msg, "reasoning_content").filter(|s| !s.is_empty());
            let signature = str_field(msg, "reasoning_signature").filter(|s| !s.is_empty());
            if let (false, Some(reasoning), Some(signature)) = (has_thinking, reasoning, signature)
            {
                blocks.insert(
                    0,
                    json!({"type": "thinking", "thinking": reasoning, "signature": signature}),
                );
            }
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tool_calls {
                    blocks.push(make_tool_use_block(tc)?);
                }
            }
        }

        if blocks.is_empty() {
            continue;
        }

        out.push(json!({"role": role, "content": blocks}));
    }

    Ok(Value::Array(out))
}

/// A `tool_result` block. A result that returned files has array content: its
/// text and, for each file, the inlined image or document or a placeholder.
fn make_tool_result_block(message: &Value, index: usize, media: &RequestMedia) -> Option<Value> {
    let tid = str_field(message, "tool_call_id").filter(|id| !id.is_empty())?;
    let content = match message.get("content")? {
        Value::String(text) => Value::String(text.clone()),
        Value::Array(parts) => Value::Array(
            parts
                .iter()
                .enumerate()
                .filter_map(|(part, block)| match Attachment::from_part(block) {
                    Some(attachment) => Some(media.part(index, part, &attachment, |_inline| true)),
                    None => (str_field(block, "type") == Some("text")).then(|| block.clone()),
                })
                .collect(),
        ),
        _ => return None,
    };
    let is_error = message.get("is_error").and_then(|v| v.as_bool()) == Some(true);
    Some(json!({
        "type": "tool_result",
        "tool_use_id": tid,
        "content": content,
        "is_error": is_error,
    }))
}

/// Returns `None` when there are no tools and rejects malformed tool JSON.
///
/// When `strict` is true, each tool gets `"strict": true` for Anthropic structured
/// outputs combined with strict tool use.
fn convert_tools_to_anthropic(
    tools_json: Option<&str>,
    strict: bool,
) -> Result<Option<Value>, Error> {
    let Some(tools_json) = tools_json.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let parsed: Value = serde_json::from_str(tools_json).map_err(|_| Error::InvalidToolsJson)?;
    let arr = parsed.as_array().ok_or(Error::InvalidToolsJson)?;

    let mut out: Vec<Value> = Vec::new();
    for item in arr {
        let (name, desc, schema) = if item.is_object() {
            if str_field(item, "type") == Some("function") {
                let function = item.get("function");
                (
                    function.and_then(|f| f.get("name")),
                    function.and_then(|f| f.get("description")),
                    function.and_then(|f| f.get("parameters")),
                )
            } else {
                (
                    item.get("name"),
                    item.get("description"),
                    item.get("input_schema"),
                )
            }
        } else {
            (None, None, None)
        };

        let name = match name.and_then(|n| n.as_str()).filter(|s| !s.is_empty()) {
            Some(n) => n,
            None => continue,
        };

        let mut tool = Map::new();
        tool.insert("name".to_string(), json!(name));
        if let Some(d) = desc.and_then(|d| d.as_str()) {
            tool.insert("description".to_string(), json!(d));
        }
        match schema {
            Some(s) => tool.insert("input_schema".to_string(), s.clone()),
            None => tool.insert("input_schema".to_string(), json!({})),
        };
        if strict {
            tool.insert("strict".to_string(), json!(true));
        }
        out.push(Value::Object(tool));
    }

    Ok(Some(Value::Array(out)))
}

#[derive(Deserialize, Serialize)]
struct AnthropicResponse {
    content: Option<Vec<AnthropicContentBlock>>,
    #[cfg(feature = "cache_profile")]
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize, Serialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

fn parse_chat_response(body: &str) -> Result<LlmResponse, Error> {
    let response: AnthropicResponse = serde_json::from_str(body).map_err(|_| Error::Parse)?;
    let content = response
        .content
        .ok_or(Error::MalformedResponse("response missing content"))?;

    let raw_message_json = serde_json::to_string(&json!({
        "role": "assistant",
        "content": &content,
    }))
    .map_err(|_| Error::Api("out of memory copying raw message"))?;

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for block in content {
        match block.kind.as_str() {
            "text" => {
                if let Some(value) = block.text {
                    text.push_str(&value);
                }
            }
            "thinking" => {
                if let Some(value) = block.thinking {
                    reasoning.push_str(&value);
                }
            }
            "tool_use" => {
                let id = block
                    .id
                    .ok_or(Error::MalformedResponse("malformed tool call"))?;
                let name = block
                    .name
                    .ok_or(Error::MalformedResponse("malformed tool call"))?;
                let arguments_json = match block.input {
                    Some(input) => serde_json::to_string(&input)
                        .map_err(|_| Error::Api("out of memory copying tool call"))?,
                    None => "{}".to_string(),
                };
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments_json,
                });
            }
            _ => {}
        }
    }

    let text_opt = (!text.is_empty()).then_some(text);
    let reasoning_opt = (!reasoning.is_empty()).then_some(reasoning);

    if text_opt.is_none() && tool_calls.is_empty() && reasoning_opt.is_none() {
        return Err(Error::EmptyResponse);
    }

    Ok(LlmResponse {
        text: text_opt,
        reasoning_content: reasoning_opt,
        raw_message_json: Some(raw_message_json),
        tool_calls,
        #[cfg(feature = "cache_profile")]
        usage: response.usage.and_then(AnthropicUsage::profile),
    })
}

/// Writes an inlined file as a base64 `image` or `document` block.
fn write_media_block(sink: &mut dyn Sink, inline: &Inline) {
    let (kind, media_type, bytes) = match inline {
        Inline::Image { media_type, bytes } => ("image", *media_type, bytes),
        Inline::Pdf(bytes) => ("document", "application/pdf", bytes),
    };
    let mut block = Object::begin(sink);
    let source = block.field("source");
    let mut source = Object::begin(source);
    write_base64_str(source.field("data"), "", bytes);
    source.str("media_type", media_type);
    source.str("type", "base64");
    source.end();
    block.str("type", kind);
    block.end();
}

/// Encodes a body object, base64-encoding the files it inlines in place.
fn encode_body(body: &Map<String, Value>, media: &RequestMedia) -> Result<BulkVec<u8>, Error> {
    encode(|sink| write_object_with_media(sink, body, media, write_media_block))
}

/// The shared request body object, without the transport-only `stream` flag.
fn chat_body_object(
    backend: &Backend,
    request: &ChatRequest<'_>,
    media: &RequestMedia,
    strict_tools: bool,
) -> Result<Map<String, Value>, Error> {
    let ChatRequest {
        system_prompt,
        messages,
        reminders,
        tools_json,
        ..
    } = *request;
    let messages = convert_messages_to_anthropic(messages, reminders, media)?;

    let mut body = backend.request_body();
    if !system_prompt.is_empty() {
        body.insert("system".to_string(), json!(system_prompt));
    }
    body.insert("messages".to_string(), messages);

    insert_tools_into_body(&mut body, tools_json, strict_tools)?;
    Ok(body)
}

fn build_chat_body(
    backend: &Backend,
    request: &ChatRequest,
    media: &RequestMedia,
) -> Result<BulkVec<u8>, Error> {
    encode_body(&chat_body_object(backend, request, media, false)?, media)
}

/// Like [`build_chat_body`] but sets `stream: true`.
fn build_stream_body(
    backend: &Backend,
    request: &ChatRequest,
    media: &RequestMedia,
) -> Result<BulkVec<u8>, Error> {
    let mut body = chat_body_object(backend, request, media, false)?;
    body.insert("stream".to_string(), json!(true));
    encode_body(&body, media)
}

fn build_chat_json_body(
    backend: &Backend,
    request: &ChatRequest<'_>,
    media: &RequestMedia,
    schema: &Value,
) -> Result<BulkVec<u8>, Error> {
    let mut body = chat_body_object(backend, request, media, true)?;
    body.insert(
        "output_config".to_string(),
        json!({
            "format": {
                "type": "json_schema",
                "schema": schema,
            }
        }),
    );

    encode_body(&body, media)
}

fn insert_tools_into_body(
    body: &mut Map<String, Value>,
    tools_json: Option<&str>,
    strict: bool,
) -> Result<(), Error> {
    if let Some(tools) = convert_tools_to_anthropic(tools_json, strict)? {
        if tools.as_array().is_some_and(|a| !a.is_empty()) {
            body.insert("tools".to_string(), tools);
            body.insert("tool_choice".to_string(), json!({"type": "auto"}));
        }
    }
    Ok(())
}

pub(super) async fn chat(
    backend: &Backend,
    http: &mut Transport<'_, impl TcpConnect, impl Dns>,
    request: &ChatRequest<'_>,
    media: &RequestMedia,
) -> Result<LlmResponse, Error> {
    let body = build_chat_body(backend, request, media)?;
    let response = post_json(http, backend, &body).await?;
    parse_chat_response(&response.body)
}

pub(super) async fn chat_json(
    backend: &Backend,
    http: &mut Transport<'_, impl TcpConnect, impl Dns>,
    request: &ChatRequest<'_>,
    media: &RequestMedia,
    _schema_name: &str,
    schema: &Value,
) -> Result<LlmResponse, Error> {
    let body = build_chat_json_body(backend, request, media, schema)?;
    let response = post_json(http, backend, &body).await?;
    parse_chat_response(&response.body)
}

pub(super) async fn chat_stream<'h, 'r>(
    backend: &Backend,
    http: &'h mut Transport<'_, impl TcpConnect, impl Dns>,
    request: &'r ChatRequest<'r>,
    media: &RequestMedia,
) -> Result<ProviderStream<'h>, Error> {
    let body = build_stream_body(backend, request, media)?;
    post_stream(
        http,
        backend,
        body,
        ProviderSse::Anthropic(AnthropicSse::new()),
    )
    .await
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn assistant(fields: Value) -> ChatMessage {
        ChatMessage::new(&fields)
    }

    fn first_block(message: ChatMessage) -> Value {
        let converted = convert_messages_to_anthropic(&[message], &[], &RequestMedia::default())
            .expect("converted");
        converted[0]["content"][0].clone()
    }

    #[test]
    fn signed_reasoning_is_replayed_as_a_thinking_block() {
        let block = first_block(assistant(json!({
            "role": "assistant",
            "content": "ok",
            "reasoning_content": "think",
            "reasoning_signature": "sig",
        })));
        assert_eq!(
            block,
            json!({"type": "thinking", "thinking": "think", "signature": "sig"})
        );
    }

    #[test]
    fn unsigned_reasoning_is_left_out() {
        let block = first_block(assistant(json!({
            "role": "assistant",
            "content": "ok",
            "reasoning_content": "think",
        })));
        assert_eq!(block, json!({"type": "text", "text": "ok"}));
    }
}

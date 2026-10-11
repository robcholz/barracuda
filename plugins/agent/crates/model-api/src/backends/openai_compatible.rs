//! OpenAI-compatible backend.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use barracuda_agent_message::attachment::has_media;
use barracuda_agent_message::json::Sink;
use barracuda_agent_message::{Attachment, ChatMessage};
use barracuda_bulk_memory::BulkVec;
use embedded_nal_async::{Dns, TcpConnect};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::attachments::{replace_parts, write_with_media, Inline, RequestMedia};
use super::super::chat_stream::ProviderStream;
use super::super::errors::Error;
use super::super::transport::Transport;
use super::super::types::{ChatRequest, LlmResponse};
use super::body::{encode, validate_tools, write_base64_str, write_compact, Object};
use super::shared::{parse_openai_chat_response, post_json, post_stream};
use super::sse::{OpenAiSse, ProviderSse};
use super::Backend;

pub(super) const CHAT_PATH: &str = "/chat/completions";

/// Optional fields of one `chat/completions` request body.
#[derive(Clone, Copy, Default)]
struct ChatExtras<'a> {
    stream: bool,
    response_format: Option<(&'a str, &'a Value)>,
}

/// Encodes a `chat/completions` body into bulk memory.
///
/// Fields are written in key order, matching the `serde_json` map encoding
/// this replaces.
fn encode_chat_body(
    backend: &Backend,
    request: &ChatRequest<'_>,
    media: &RequestMedia,
    extras: ChatExtras<'_>,
) -> Result<BulkVec<u8>, Error> {
    let tools = request.tools_json.filter(|tools| !tools.is_empty());
    if let Some(tools) = tools {
        validate_tools(tools)?;
    }
    let outgoing = outgoing_messages(request, media);
    encode(|sink| {
        let mut body = Object::begin(sink);
        body.value("max_tokens", &Value::from(backend.max_tokens));
        let messages = body.field("messages");
        messages.put(b"[");
        let mut first = true;
        if !request.system_prompt.is_empty() {
            let mut system = Object::begin(messages);
            system.str("content", request.system_prompt);
            system.str("role", "system");
            system.end();
            first = false;
        }
        for message in &outgoing {
            if !first {
                messages.put(b",");
            }
            first = false;
            match message {
                Outgoing::Stored(message) => messages.put(message.as_bytes()),
                Outgoing::Converted(message) => {
                    write_with_media(messages, message, media, write_image_part);
                }
            }
        }
        messages.put(b"]");
        body.str("model", &backend.model);
        if let Some((name, schema)) = extras.response_format {
            let format = body.field("response_format");
            let mut format = Object::begin(format);
            let json_schema = format.field("json_schema");
            let mut json_schema = Object::begin(json_schema);
            json_schema.str("name", name);
            json_schema.value("schema", schema);
            json_schema.value("strict", &Value::Bool(true));
            json_schema.end();
            format.str("type", "json_schema");
            format.end();
        }
        if extras.stream {
            body.value("stream", &Value::Bool(true));
            #[cfg(feature = "cache_profile")]
            {
                let options = body.field("stream_options");
                let mut options = Object::begin(options);
                options.value("include_usage", &Value::Bool(true));
                options.end();
            }
        }
        if let Some(tools) = tools {
            write_compact(body.field("tools"), tools);
        }
        body.end();
    })
}

/// One message of the request body.
enum Outgoing<'a> {
    /// Copied as stored.
    Stored(&'a ChatMessage),
    /// Decoded to replace its `media` parts, or added for tool images.
    Converted(Value),
}

/// The text that opens the user message carrying tool results' images.
const TOOL_IMAGES_LEAD: &str = "The tool results above returned these images.";

#[derive(Deserialize)]
struct Role<'a> {
    #[serde(borrow)]
    role: Option<&'a str>,
}

/// The request's messages with attachments resolved.
///
/// A user message's `media` parts become inlined images or placeholders. A
/// tool message stays text, since tool messages take no images: each part
/// becomes a line, an inlined image its label. The images then follow the run
/// of tool results in one added user message.
fn outgoing_messages<'a>(request: &ChatRequest<'a>, media: &RequestMedia) -> Vec<Outgoing<'a>> {
    let mut outgoing = Vec::new();
    let mut tool_images = Vec::new();
    for (index, message) in request.messages.iter().chain(request.reminders).enumerate() {
        let role = message.parse::<Role<'_>>().ok().and_then(|role| role.role);
        if !tool_images.is_empty() && role != Some("tool") {
            outgoing.push(Outgoing::Converted(tool_images_message(&mut tool_images)));
        }
        if !has_media(message.as_str()) {
            outgoing.push(Outgoing::Stored(message));
            continue;
        }
        let mut value = message.to_value();
        if role == Some("tool") {
            flatten_tool_content(&mut value, index, media, &mut tool_images);
        } else {
            replace_parts(&mut value, index, media, is_image);
        }
        outgoing.push(Outgoing::Converted(value));
    }
    if !tool_images.is_empty() {
        outgoing.push(Outgoing::Converted(tool_images_message(&mut tool_images)));
    }
    outgoing
}

/// Replaces a tool message's content array with its text, moving the label
/// and marker of each inlined image to `images`.
fn flatten_tool_content(
    message: &mut Value,
    index: usize,
    media: &RequestMedia,
    images: &mut Vec<Value>,
) {
    let Some(parts) = message.get("content").and_then(Value::as_array) else {
        return;
    };
    let mut lines = Vec::new();
    for (part, content) in parts.iter().enumerate() {
        match Attachment::from_part(content) {
            Some(attachment) => {
                let replacement = media.part(index, part, &attachment, is_image);
                match replacement.get("text").and_then(Value::as_str) {
                    Some(placeholder) => lines.push(placeholder.to_owned()),
                    None => {
                        let label = attachment.label();
                        images.push(json!({"text": label, "type": "text"}));
                        images.push(replacement);
                        lines.push(label);
                    }
                }
            }
            None => {
                if let Some(text) = content.get("text").and_then(Value::as_str) {
                    lines.push(text.to_owned());
                }
            }
        }
    }
    if let Some(content) = message.get_mut("content") {
        *content = Value::String(lines.join("\n"));
    }
}

/// The user message carrying the images of the tool results before it.
fn tool_images_message(images: &mut Vec<Value>) -> Value {
    let mut content = vec![json!({"text": TOOL_IMAGES_LEAD, "type": "text"})];
    content.append(images);
    json!({"content": content, "role": "user"})
}

fn is_image(inline: &Inline) -> bool {
    matches!(inline, Inline::Image { .. })
}

/// Writes an inlined image as an `image_url` part with a data URL.
fn write_image_part(sink: &mut dyn Sink, inline: &Inline) {
    let mut part = Object::begin(sink);
    if let Inline::Image { media_type, bytes } = inline {
        let image_url = part.field("image_url");
        let mut url = Object::begin(image_url);
        write_base64_str(
            url.field("url"),
            &format!("data:{media_type};base64,"),
            bytes,
        );
        url.end();
        part.str("type", "image_url");
    }
    part.end();
}

fn build_chat_body(
    backend: &Backend,
    request: &ChatRequest,
    media: &RequestMedia,
) -> Result<BulkVec<u8>, Error> {
    encode_chat_body(backend, request, media, ChatExtras::default())
}

/// Like [`build_chat_body`] but sets `stream: true` so the provider replies
/// with a `text/event-stream` body.
fn build_stream_body(
    backend: &Backend,
    request: &ChatRequest,
    media: &RequestMedia,
) -> Result<BulkVec<u8>, Error> {
    encode_chat_body(
        backend,
        request,
        media,
        ChatExtras {
            stream: true,
            ..ChatExtras::default()
        },
    )
}

fn build_chat_json_body(
    backend: &Backend,
    request: &ChatRequest<'_>,
    media: &RequestMedia,
    schema_name: &str,
    schema: &Value,
) -> Result<BulkVec<u8>, Error> {
    encode_chat_body(
        backend,
        request,
        media,
        ChatExtras {
            response_format: Some((schema_name, schema)),
            ..ChatExtras::default()
        },
    )
}

pub(super) async fn chat(
    backend: &Backend,
    http: &mut Transport<'_, impl TcpConnect, impl Dns>,
    request: &ChatRequest<'_>,
    media: &RequestMedia,
) -> Result<LlmResponse, Error> {
    let body = build_chat_body(backend, request, media)?;
    let response = post_json(http, backend, &body).await?;
    parse_openai_chat_response(&response.body)
}

pub(super) async fn chat_json(
    backend: &Backend,
    http: &mut Transport<'_, impl TcpConnect, impl Dns>,
    request: &ChatRequest<'_>,
    media: &RequestMedia,
    schema_name: &str,
    schema: &Value,
) -> Result<LlmResponse, Error> {
    let body = build_chat_json_body(backend, request, media, schema_name, schema)?;
    let response = post_json(http, backend, &body).await?;
    parse_openai_chat_response(&response.body)
}

pub(super) async fn chat_stream<'h, 'r>(
    backend: &Backend,
    http: &'h mut Transport<'_, impl TcpConnect, impl Dns>,
    request: &'r ChatRequest<'r>,
    media: &RequestMedia,
) -> Result<ProviderStream<'h>, Error> {
    let body = build_stream_body(backend, request, media)?;
    post_stream(http, backend, body, ProviderSse::OpenAi(OpenAiSse::new())).await
}

#[cfg(all(test, feature = "cache_profile"))]
mod tests {
    use super::*;
    use crate::{BackendKind, ModelApiConfig};

    #[test]
    fn streaming_requests_ask_provider_to_include_usage() {
        let backend = BackendKind::OpenAiCompatible.make(ModelApiConfig::new(
            BackendKind::OpenAiCompatible,
            "key",
            "model",
            "https://example.invalid/v1",
        ));
        let messages = [];
        let body = build_stream_body(
            &backend,
            &ChatRequest::new("system", &messages),
            &RequestMedia::default(),
        )
        .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }
}

#[cfg(test)]
mod wire_tests {
    #![allow(clippy::unwrap_used)]

    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use serde_json::{json, Map};

    use super::*;
    use crate::ChatMessage;
    use crate::{BackendKind, ModelApiConfig};

    fn backend() -> Backend {
        BackendKind::OpenAiCompatible.make(ModelApiConfig::new(
            BackendKind::OpenAiCompatible,
            "key",
            "model-\"x\"",
            "https://example.invalid/v1",
        ))
    }

    /// The `serde_json` map encoding the bulk encoder replaced.
    fn reference(backend: &Backend, request: &ChatRequest<'_>, extra: &[(&str, Value)]) -> String {
        let mut messages = Vec::new();
        if !request.system_prompt.is_empty() {
            messages.push(json!({"role": "system", "content": request.system_prompt}));
        }
        messages.extend(request.messages.iter().map(ChatMessage::to_value));
        messages.extend(request.reminders.iter().map(ChatMessage::to_value));
        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(backend.model.clone()));
        body.insert("max_tokens".to_string(), Value::from(backend.max_tokens));
        body.insert("messages".to_string(), Value::Array(messages));
        if let Some(tools) = request.tools_json.filter(|tools| !tools.is_empty()) {
            body.insert(
                "tools".to_string(),
                serde_json::from_str::<Value>(tools).unwrap(),
            );
        }
        for (name, value) in extra {
            body.insert((*name).to_string(), value.clone());
        }
        serde_json::to_string(&Value::Object(body)).unwrap()
    }

    fn text(body: &BulkVec<u8>) -> &str {
        core::str::from_utf8(body.as_slice()).unwrap()
    }

    fn history() -> Vec<ChatMessage> {
        [
            json!({"role": "user", "content": "line\nbreak \"quoted\" ünï 🚀"}),
            json!({"role": "assistant", "content": null, "reasoning_content": "hm",
                   "tool_calls": [{"id": "c1", "type": "function",
                                   "function": {"name": "time_now", "arguments": "{\"tz\":\"utc\"}"}}]}),
            json!({"role": "tool", "tool_call_id": "c1", "content": "{\"utc\":1.5e3}"}),
        ]
        .iter()
        .map(ChatMessage::new)
        .collect()
    }

    #[test]
    fn chat_bodies_match_the_previous_encoding_byte_for_byte() {
        let backend = backend();
        let messages = history();
        let reminders = [ChatMessage::new(
            &json!({"role": "user", "content": "<system-reminder>\nx\n</system-reminder>"}),
        )];
        let tools = r#"[{"function":{"description":"d","name":"time_now","parameters":{"properties":{},"type":"object"}},"type":"function"}]"#;
        let mut request = ChatRequest::new("system \t prompt", &messages);
        request.reminders = &reminders;
        request.tools_json = Some(tools);

        assert_eq!(
            text(&build_chat_body(&backend, &request, &RequestMedia::default()).unwrap()),
            reference(&backend, &request, &[])
        );

        let mut stream_extra = vec![("stream", json!(true))];
        if cfg!(feature = "cache_profile") {
            stream_extra.push(("stream_options", json!({"include_usage": true})));
        }
        assert_eq!(
            text(&build_stream_body(&backend, &request, &RequestMedia::default()).unwrap()),
            reference(&backend, &request, &stream_extra)
        );

        let schema = json!({"type": "object", "properties": {"a": {"type": "number"}}});
        let format = json!({"type": "json_schema",
                            "json_schema": {"name": "answer", "strict": true, "schema": schema}});
        assert_eq!(
            text(
                &build_chat_json_body(
                    &backend,
                    &request,
                    &RequestMedia::default(),
                    "answer",
                    &schema
                )
                .unwrap()
            ),
            reference(&backend, &request, &[("response_format", format)])
        );

        let empty = ChatRequest::new("", &[]);
        assert_eq!(
            text(&build_chat_body(&backend, &empty, &RequestMedia::default()).unwrap()),
            reference(&backend, &empty, &[])
        );
    }

    #[test]
    fn pretty_tools_are_sent_compact_and_invalid_tools_are_rejected() {
        let backend = backend();
        let pretty =
            "[\n  {\n    \"type\": \"function\",\n    \"function\": {\"name\": \"a b\"}\n  }\n]";
        let mut request = ChatRequest::new("s", &[]);
        request.tools_json = Some(pretty);
        let body: Value = serde_json::from_slice(
            &build_chat_body(&backend, &request, &RequestMedia::default()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            body["tools"],
            serde_json::from_str::<Value>(pretty).unwrap()
        );
        assert!(
            !text(&build_chat_body(&backend, &request, &RequestMedia::default()).unwrap())
                .contains('\n')
        );

        request.tools_json = Some("{\"not\":\"an array\"}");
        assert!(matches!(
            build_chat_body(&backend, &request, &RequestMedia::default()),
            Err(Error::InvalidToolsJson)
        ));
    }
}

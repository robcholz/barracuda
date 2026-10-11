//! OpenAI-compatible backend.

use alloc::format;
use alloc::vec::Vec;

use barracuda_agent_message::attachment::has_media;
use barracuda_agent_message::json::Sink;
use barracuda_bulk_memory::BulkVec;
use embedded_nal_async::{Dns, TcpConnect};
use serde_json::Value;

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
    let converted = convert_media_messages(request, media);
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
        for (message, converted) in request
            .messages
            .iter()
            .chain(request.reminders)
            .zip(&converted)
        {
            if !first {
                messages.put(b",");
            }
            first = false;
            match converted {
                Some(converted) => write_with_media(messages, converted, media, write_image_part),
                None => messages.put(message.as_bytes()),
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

/// Decodes each message that references attachments, with its `media` parts
/// replaced by inlined images or placeholders; other messages stay `None` and
/// are copied as stored.
fn convert_media_messages(request: &ChatRequest<'_>, media: &RequestMedia) -> Vec<Option<Value>> {
    request
        .messages
        .iter()
        .chain(request.reminders)
        .enumerate()
        .map(|(index, message)| {
            has_media(message.as_str()).then(|| {
                let mut value = message.to_value();
                replace_parts(&mut value, index, media, is_image);
                value
            })
        })
        .collect()
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

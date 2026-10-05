//! OpenAI-compatible backend.

use alloc::format;
use alloc::string::String;

use barracuda_bulk_memory::BulkVec;
use embedded_nal_async::{Dns, TcpConnect};
use serde_json::Value;

use super::super::chat_stream::ProviderStream;
use super::super::errors::Error;
use super::super::media::{prepare_asset, Prepared};
use super::super::transport::Transport;
use super::super::types::{ChatRequest, LlmResponse, MediaRequest};
use super::body::{encode, validate_tools, write_base64_str, write_compact, write_str, Object};
use super::shared::{media_text, parse_openai_chat_response, post_json, post_stream};
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
    extras: ChatExtras<'_>,
) -> Result<BulkVec<u8>, Error> {
    let tools = request.tools_json.filter(|tools| !tools.is_empty());
    if let Some(tools) = tools {
        validate_tools(tools)?;
    }
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
        for message in request.messages.iter().chain(request.reminders) {
            if !first {
                messages.put(b",");
            }
            first = false;
            messages.put(message.as_bytes());
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

fn build_chat_body(backend: &Backend, request: &ChatRequest) -> Result<BulkVec<u8>, Error> {
    encode_chat_body(backend, request, ChatExtras::default())
}

/// Like [`build_chat_body`] but sets `stream: true` so the provider replies
/// with a `text/event-stream` body.
fn build_stream_body(backend: &Backend, request: &ChatRequest) -> Result<BulkVec<u8>, Error> {
    encode_chat_body(
        backend,
        request,
        ChatExtras {
            stream: true,
            ..ChatExtras::default()
        },
    )
}

fn build_chat_json_body(
    backend: &Backend,
    request: &ChatRequest<'_>,
    schema_name: &str,
    schema: &Value,
) -> Result<BulkVec<u8>, Error> {
    encode_chat_body(
        backend,
        request,
        ChatExtras {
            response_format: Some((schema_name, schema)),
            ..ChatExtras::default()
        },
    )
}

/// Encodes the media inference request body (no transport).
fn build_media_body(backend: &Backend, request: &MediaRequest<'_>) -> Result<BulkVec<u8>, Error> {
    let Some(user_prompt) = request.user_prompt.filter(|prompt| !prompt.is_empty()) else {
        return Err(Error::IncompleteMediaRequest);
    };
    let prepared = prepare_asset(request.media, backend.image_max_bytes)?;
    let system_prompt = request.system_prompt.filter(|prompt| !prompt.is_empty());
    encode(|sink| {
        let mut body = Object::begin(sink);
        body.value("max_tokens", &Value::from(backend.max_tokens));
        let messages = body.field("messages");
        messages.put(b"[");
        if let Some(system) = system_prompt {
            let mut message = Object::begin(messages);
            message.str("content", system);
            message.str("role", "system");
            message.end();
            messages.put(b",");
        }
        let mut message = Object::begin(messages);
        let content = message.field("content");
        content.put(b"[");
        let mut text = Object::begin(content);
        text.str("text", user_prompt);
        text.str("type", "text");
        text.end();
        content.put(b",");
        let mut image = Object::begin(content);
        let image_url = image.field("image_url");
        let mut url = Object::begin(image_url);
        let target = url.field("url");
        match prepared {
            Prepared::Inline { mime_type, bytes } => {
                write_base64_str(target, &format!("data:{mime_type};base64,"), bytes);
            }
            Prepared::RemoteUrl(remote) => write_str(target, remote),
        }
        url.end();
        image.str("type", "image_url");
        image.end();
        content.put(b"]");
        message.str("role", "user");
        message.end();
        messages.put(b"]");
        body.str("model", &backend.model);
        body.end();
    })
}

pub(super) async fn chat(
    backend: &Backend,
    http: &mut Transport<'_, impl TcpConnect, impl Dns>,
    request: &ChatRequest<'_>,
) -> Result<LlmResponse, Error> {
    let body = build_chat_body(backend, request)?;
    let response = post_json(http, backend, &body).await?;
    parse_openai_chat_response(&response.body)
}

pub(super) async fn chat_json(
    backend: &Backend,
    http: &mut Transport<'_, impl TcpConnect, impl Dns>,
    request: &ChatRequest<'_>,
    schema_name: &str,
    schema: &Value,
) -> Result<LlmResponse, Error> {
    let body = build_chat_json_body(backend, request, schema_name, schema)?;
    let response = post_json(http, backend, &body).await?;
    parse_openai_chat_response(&response.body)
}

pub(super) async fn infer_media(
    backend: &Backend,
    http: &mut Transport<'_, impl TcpConnect, impl Dns>,
    request: &MediaRequest<'_>,
) -> Result<String, Error> {
    let body = build_media_body(backend, request)?;
    let response = post_json(http, backend, &body).await?;
    media_text(parse_openai_chat_response(&response.body)?)
}

pub(super) async fn chat_stream<'h, 'r>(
    backend: &Backend,
    http: &'h mut Transport<'_, impl TcpConnect, impl Dns>,
    request: &'r ChatRequest<'r>,
) -> Result<ProviderStream<'h>, Error> {
    let body = build_stream_body(backend, request)?;
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
        let body = build_stream_body(&backend, &ChatRequest::new("system", &messages)).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }
}

#[cfg(test)]
mod wire_tests {
    #![allow(clippy::unwrap_used)]

    use alloc::string::ToString;
    use alloc::vec;
    use alloc::vec::Vec;

    use serde_json::{json, Map};

    use super::*;
    use crate::types::MediaAsset;
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
            text(&build_chat_body(&backend, &request).unwrap()),
            reference(&backend, &request, &[])
        );

        let mut stream_extra = vec![("stream", json!(true))];
        if cfg!(feature = "cache_profile") {
            stream_extra.push(("stream_options", json!({"include_usage": true})));
        }
        assert_eq!(
            text(&build_stream_body(&backend, &request).unwrap()),
            reference(&backend, &request, &stream_extra)
        );

        let schema = json!({"type": "object", "properties": {"a": {"type": "number"}}});
        let format = json!({"type": "json_schema",
                            "json_schema": {"name": "answer", "strict": true, "schema": schema}});
        assert_eq!(
            text(&build_chat_json_body(&backend, &request, "answer", &schema).unwrap()),
            reference(&backend, &request, &[("response_format", format)])
        );

        let empty = ChatRequest::new("", &[]);
        assert_eq!(
            text(&build_chat_body(&backend, &empty).unwrap()),
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
        let body: Value =
            serde_json::from_slice(&build_chat_body(&backend, &request).unwrap()).unwrap();
        assert_eq!(
            body["tools"],
            serde_json::from_str::<Value>(pretty).unwrap()
        );
        assert!(!text(&build_chat_body(&backend, &request).unwrap()).contains('\n'));

        request.tools_json = Some("{\"not\":\"an array\"}");
        assert!(matches!(
            build_chat_body(&backend, &request),
            Err(Error::InvalidToolsJson)
        ));
    }

    #[test]
    fn media_body_matches_the_previous_encoding() {
        let backend = backend();
        let asset = MediaAsset::inline_bytes(vec![0_u8, 1, 2, 250, 251, 252, 253], "image/png");
        let mut request = MediaRequest::new(&asset);
        request.system_prompt = Some("sys");
        request.user_prompt = Some("what is \"this\"?");
        let base64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            [0_u8, 1, 2, 250, 251, 252, 253],
        );
        let expected = serde_json::to_string(&json!({
            "model": backend.model,
            "max_tokens": backend.max_tokens,
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": [
                    {"type": "text", "text": "what is \"this\"?"},
                    {"type": "image_url", "image_url": {"url": alloc::format!("data:image/png;base64,{base64}")}}
                ]}
            ]
        }))
        .unwrap();
        assert_eq!(
            text(&build_media_body(&backend, &request).unwrap()),
            expected
        );
    }
}

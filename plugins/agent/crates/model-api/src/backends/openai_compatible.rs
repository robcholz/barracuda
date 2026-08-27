//! OpenAI-compatible backend.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embedded_nal_async::{Dns, TcpConnect};
use serde_json::{json, Value};

use super::super::chat_stream::ProviderStream;
use super::super::errors::Error;
use super::super::media::prepare_asset;
use super::super::transport::Transport;
use super::super::types::{ChatRequest, LlmResponse, MediaRequest};
use super::shared::{
    insert_tools_into_body, media_text, parse_openai_chat_response, post_json, post_stream,
    serialize_chat_body,
};
use super::sse::{OpenAiSse, ProviderSse};
use super::Backend;

pub(super) const CHAT_PATH: &str = "/chat/completions";

/// The shared request body object for `chat/completions`, without the
/// transport-only `stream` flag.
fn chat_body_object(
    backend: &Backend,
    system_prompt: &str,
    messages: &[Value],
    reminders: &[Value],
    tools_json: Option<&str>,
) -> Result<serde_json::Map<String, Value>, Error> {
    let mut output_messages = Vec::new();
    if !system_prompt.is_empty() {
        output_messages.push(json!({"role": "system", "content": system_prompt}));
    }
    output_messages.extend(messages.iter().cloned());
    output_messages.extend(reminders.iter().cloned());

    let mut body = backend.request_body();
    body.insert("messages".to_string(), Value::Array(output_messages));

    if let Some(tools_json) = tools_json.filter(|s| !s.is_empty()) {
        insert_tools_into_body(&mut body, tools_json)?;
    }
    Ok(body)
}

fn build_chat_body(backend: &Backend, request: &ChatRequest) -> Result<String, Error> {
    serialize_chat_body(chat_body_object(
        backend,
        request.system_prompt,
        request.messages,
        request.reminders,
        request.tools_json,
    )?)
}

/// Like [`build_chat_body`] but sets `stream: true` so the provider replies
/// with a `text/event-stream` body.
fn build_stream_body(backend: &Backend, request: &ChatRequest) -> Result<String, Error> {
    let mut body = chat_body_object(
        backend,
        request.system_prompt,
        request.messages,
        request.reminders,
        request.tools_json,
    )?;
    body.insert("stream".to_string(), json!(true));
    #[cfg(feature = "cache_profile")]
    body.insert(
        "stream_options".to_string(),
        json!({ "include_usage": true }),
    );
    serialize_chat_body(body)
}

fn build_chat_json_body(
    backend: &Backend,
    request: &ChatRequest<'_>,
    schema_name: &str,
    schema: &Value,
) -> Result<String, Error> {
    let mut body = chat_body_object(
        backend,
        request.system_prompt,
        request.messages,
        request.reminders,
        request.tools_json,
    )?;
    body.insert(
        "response_format".to_string(),
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": schema_name,
                "strict": true,
                "schema": schema,
            }
        }),
    );
    serialize_chat_body(body)
}

/// Serialize the media inference request body (no transport).
fn build_media_body(backend: &Backend, request: &MediaRequest<'_>) -> Result<String, Error> {
    let Some(user_prompt) = request.user_prompt.filter(|prompt| !prompt.is_empty()) else {
        return Err(Error::IncompleteMediaRequest);
    };
    let prepared = prepare_asset(request.media, backend.image_max_bytes)?;
    let image_url = prepared.openai_url();

    let mut body = backend.request_body();
    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = request.system_prompt.filter(|prompt| !prompt.is_empty()) {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.push(json!({"role": "user", "content": [
        {"type": "text", "text": user_prompt},
            {"type": "image_url", "image_url": {"url": image_url.as_ref()}}
    ]}));
    body.insert("messages".to_string(), Value::Array(messages));

    serde_json::to_string(&Value::Object(body))
        .map_err(|_| Error::Api("out of memory serializing media request"))
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
        let body: Value = serde_json::from_str(&body).unwrap();

        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }
}

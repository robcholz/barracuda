//! OpenAI-compatible backend, port of `claw_llm_backend_openai_compatible.c`.

use core::sync::atomic::AtomicBool;

use serde_json::{json, Value};

use claw_interface::http::{
    blocking::ClawHttp as BlockingClawHttp, Cancel, ClawHttp, StreamingHttp,
};

use super::super::chat_stream::ProviderStream;
use super::super::errors::{ChatError, ClawApiError, InferMediaError, InitError};
use super::super::media::prepare_asset;
use super::super::types::{ChatJsonRequest, ChatRequest, ClawApiConfig, LlmResponse, MediaRequest};
use super::shared::{
    insert_tools_into_body, media_text, parse_openai_chat_response, post_prepared,
    post_prepared_async, post_prepared_stream, single_media_asset, BackendContext, PreparedAuth,
    PreparedRequest,
};
use super::sse::{OpenAiSse, ProviderSse};
use super::BackendImpl;

/// Chat endpoint path appended to the base URL.
const CHAT_PATH: &str = "/chat/completions";
/// Provider field name that carries the max-tokens value.
const MAX_TOKENS_FIELD: &str = "max_tokens";
/// Whether media prep must reject local/inline images (remote URLs only).
const IMAGE_REMOTE_URL_ONLY: bool = false;

pub(super) struct OpenAiCompatible {
    context: BackendContext,
}

impl OpenAiCompatible {
    /// The shared request body object for `chat/completions`, without the
    /// transport-only `stream` flag.
    fn chat_body_object(
        &self,
        request: &ChatRequest,
    ) -> Result<serde_json::Map<String, Value>, ChatError> {
        let mut messages: Vec<Value> = Vec::new();
        if !request.system_prompt.is_empty() {
            messages.push(json!({"role": "system", "content": request.system_prompt}));
        }
        if let Some(arr) = request.messages.as_array() {
            messages.extend(arr.iter().cloned());
        }
        // Ephemeral trailing reminders, appended after the persisted history.
        messages.extend(request.reminders.iter().cloned());

        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), json!(self.context.model()));
        body.insert(
            MAX_TOKENS_FIELD.to_string(),
            json!(self.context.max_tokens()),
        );
        body.insert("messages".to_string(), Value::Array(messages));

        if let Some(tools_json) = request.tools_json.filter(|s| !s.is_empty()) {
            insert_tools_into_body(&mut body, tools_json)?;
        }
        Ok(body)
    }

    /// `build_chat_body`
    fn build_chat_body(&self, request: &ChatRequest) -> Result<String, ChatError> {
        serialize_body(self.chat_body_object(request)?)
    }

    /// Like [`build_chat_body`](Self::build_chat_body) but sets `stream: true` so
    /// the provider replies with a `text/event-stream` body.
    fn build_stream_body(&self, request: &ChatRequest) -> Result<String, ChatError> {
        let mut body = self.chat_body_object(request)?;
        body.insert("stream".to_string(), json!(true));
        #[cfg(feature = "cache_profile")]
        body.insert(
            "stream_options".to_string(),
            json!({ "include_usage": true }),
        );
        serialize_body(body)
    }

    fn build_chat_json_body(
        &self,
        request: &ChatJsonRequest<'_>,
        schema_name: &str,
        schema: &Value,
    ) -> Result<String, ChatError> {
        let mut messages: Vec<Value> = Vec::new();
        if !request.system_prompt.is_empty() {
            messages.push(json!({"role": "system", "content": request.system_prompt}));
        }
        if let Some(arr) = request.messages.as_array() {
            messages.extend(arr.iter().cloned());
        }
        // Ephemeral trailing reminders, appended after the persisted history.
        messages.extend(request.reminders.iter().cloned());

        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), json!(self.context.model()));
        body.insert(
            MAX_TOKENS_FIELD.to_string(),
            json!(self.context.max_tokens()),
        );
        body.insert("messages".to_string(), Value::Array(messages));
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
        if let Some(tools_json) = request.tools_json.filter(|s| !s.is_empty()) {
            insert_tools_into_body(&mut body, tools_json)?;
        }

        serde_json::to_string(&Value::Object(body)).map_err(|_| {
            ChatError::Api(ClawApiError::ApiError("out of memory serializing request"))
        })
    }

    /// Bearer auth carrying this backend's configured API key.
    fn auth(&self) -> PreparedAuth {
        PreparedAuth::Bearer(self.context.api_key().to_string())
    }

    fn prepare_chat(&self, request: &ChatRequest<'_>) -> Result<PreparedRequest, ChatError> {
        let body = self.build_chat_body(request)?;
        Ok(self
            .context
            .prepare(CHAT_PATH, body, self.auth(), Vec::new()))
    }

    fn prepare_chat_json(
        &self,
        request: &ChatJsonRequest<'_>,
        schema_name: &str,
        schema: &Value,
    ) -> Result<PreparedRequest, ChatError> {
        let body = self.build_chat_json_body(request, schema_name, schema)?;
        Ok(self
            .context
            .prepare(CHAT_PATH, body, self.auth(), Vec::new()))
    }

    fn prepare_stream(&self, request: &ChatRequest<'_>) -> Result<PreparedRequest, ChatError> {
        let body = self.build_stream_body(request)?;
        Ok(self
            .context
            .prepare(CHAT_PATH, body, self.auth(), Vec::new()))
    }

    /// Serialize the media inference request body (no transport).
    fn build_media_body(&self, request: &MediaRequest<'_>) -> Result<String, InferMediaError> {
        let Some(user_prompt) = request.user_prompt.filter(|prompt| !prompt.is_empty()) else {
            return Err(InferMediaError::IncompleteRequest);
        };
        let asset = single_media_asset(request.media)?;

        let prepared = prepare_asset(asset, IMAGE_REMOTE_URL_ONLY, self.context.image_max_bytes())?;

        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), json!(self.context.model()));
        body.insert(
            MAX_TOKENS_FIELD.to_string(),
            json!(self.context.max_tokens()),
        );
        let mut messages: Vec<Value> = Vec::new();
        if let Some(system) = request.system_prompt.filter(|prompt| !prompt.is_empty()) {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.push(json!({"role": "user", "content": [
            {"type": "text", "text": user_prompt},
                {"type": "image_url", "image_url": {"url": prepared.payload()}}
        ]}));
        body.insert("messages".to_string(), Value::Array(messages));

        serde_json::to_string(&Value::Object(body))
            .map_err(|_| ClawApiError::ApiError("out of memory serializing media request").into())
    }

    fn prepare_media(&self, request: &MediaRequest<'_>) -> Result<PreparedRequest, InferMediaError> {
        let body = self.build_media_body(request)?;
        Ok(self
            .context
            .prepare(CHAT_PATH, body, self.auth(), Vec::new()))
    }
}

impl BackendImpl for OpenAiCompatible {
    /// `openai_compatible_init`
    fn make(config: &ClawApiConfig) -> Result<Self, InitError> {
        Ok(OpenAiCompatible {
            context: BackendContext::from_config(config),
        })
    }

    /// `openai_compatible_chat`
    fn chat<H: BlockingClawHttp>(
        &self,
        http: &mut H,
        request: &ChatRequest,
        abort: &AtomicBool,
    ) -> Result<LlmResponse, ChatError> {
        let prepared = self.prepare_chat(request)?;
        let response = post_prepared(http, &prepared, abort)?;
        Ok(parse_openai_chat_response(&response.body)?)
    }

    fn chat_json<H: BlockingClawHttp>(
        &self,
        http: &mut H,
        request: &ChatJsonRequest<'_>,
        schema_name: &str,
        schema: &Value,
        abort: &AtomicBool,
    ) -> Result<LlmResponse, ChatError> {
        let prepared = self.prepare_chat_json(request, schema_name, schema)?;
        let response = post_prepared(http, &prepared, abort)?;
        Ok(parse_openai_chat_response(&response.body)?)
    }

    /// `openai_compatible_infer_media`
    fn infer_media<H: BlockingClawHttp>(
        &self,
        http: &mut H,
        request: &MediaRequest,
        abort: &AtomicBool,
    ) -> Result<String, InferMediaError> {
        let prepared = self.prepare_media(request)?;
        let response = post_prepared(http, &prepared, abort)?;
        media_text(parse_openai_chat_response(&response.body)?)
    }

    async fn chat_async<H: ClawHttp>(
        &self,
        http: &mut H,
        request: &ChatRequest<'_>,
        cancel: Cancel<'_>,
    ) -> Result<LlmResponse, ChatError> {
        let prepared = self.prepare_chat(request)?;
        let response = post_prepared_async(http, &prepared, cancel).await?;
        Ok(parse_openai_chat_response(&response.body)?)
    }

    async fn chat_json_async<H: ClawHttp>(
        &self,
        http: &mut H,
        request: &ChatJsonRequest<'_>,
        schema_name: &str,
        schema: &Value,
        cancel: Cancel<'_>,
    ) -> Result<LlmResponse, ChatError> {
        let prepared = self.prepare_chat_json(request, schema_name, schema)?;
        let response = post_prepared_async(http, &prepared, cancel).await?;
        Ok(parse_openai_chat_response(&response.body)?)
    }

    async fn infer_media_async<H: ClawHttp>(
        &self,
        http: &mut H,
        request: &MediaRequest<'_>,
        cancel: Cancel<'_>,
    ) -> Result<String, InferMediaError> {
        let prepared = self.prepare_media(request)?;
        let response = post_prepared_async(http, &prepared, cancel).await?;
        media_text(parse_openai_chat_response(&response.body)?)
    }

    async fn chat_stream_async<'h, 'r, H: StreamingHttp>(
        &self,
        http: &'h mut H,
        request: &'r ChatRequest<'r>,
        cancel: Cancel<'h>,
    ) -> Result<ProviderStream<H::ByteStream<'h>>, ChatError> {
        let prepared = self.prepare_stream(request)?;
        post_prepared_stream(http, &prepared, cancel, ProviderSse::OpenAi(OpenAiSse::new())).await
    }
}

fn serialize_body(body: serde_json::Map<String, Value>) -> Result<String, ChatError> {
    serde_json::to_string(&Value::Object(body))
        .map_err(|_| ChatError::Api(ClawApiError::ApiError("out of memory serializing request")))
}

#[cfg(all(test, feature = "cache_profile"))]
mod tests {
    use super::*;
    use crate::BackendKind;

    #[test]
    fn streaming_requests_ask_provider_to_include_usage() {
        let backend = OpenAiCompatible::make(&ClawApiConfig::new(
            BackendKind::OpenAiCompatible,
            "key",
            "model",
            "https://example.invalid/v1",
        ))
        .unwrap();
        let messages = serde_json::json!([]);
        let body = backend
            .build_stream_body(&ChatRequest::new("system", &messages))
            .unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();

        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }
}

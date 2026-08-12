//! OpenAI-compatible backend.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use serde_json::{json, Value};

use embedded_nal_async::{Dns, TcpConnect};

use super::super::chat_stream::ProviderStream;
use super::super::errors::{ChatError, ClawApiError, InferMediaError};
use super::super::media::prepare_asset;
use super::super::transport::{HttpTransport as NetClient, ResponseStream};
use super::super::types::{ChatJsonRequest, ChatRequest, ClawApiConfig, LlmResponse, MediaRequest};
use super::shared::{
    insert_tools_into_body, media_text, parse_openai_chat_response, post_json, post_stream,
    serialize_chat_body, BackendContext,
};
use super::sse::{OpenAiSse, ProviderSse};

const CHAT_PATH: &str = "/chat/completions";

pub(crate) struct OpenAiCompatible {
    context: BackendContext,
}

impl OpenAiCompatible {
    /// The shared request body object for `chat/completions`, without the
    /// transport-only `stream` flag.
    fn chat_body_object(
        &self,
        system_prompt: &str,
        messages: &[Value],
        reminders: &[Value],
        tools_json: Option<&str>,
    ) -> Result<serde_json::Map<String, Value>, ChatError> {
        let mut output_messages = Vec::new();
        if !system_prompt.is_empty() {
            output_messages.push(json!({"role": "system", "content": system_prompt}));
        }
        output_messages.extend(messages.iter().cloned());
        output_messages.extend(reminders.iter().cloned());

        let mut body = self.context.request_body();
        body.insert("messages".to_string(), Value::Array(output_messages));

        if let Some(tools_json) = tools_json.filter(|s| !s.is_empty()) {
            insert_tools_into_body(&mut body, tools_json)?;
        }
        Ok(body)
    }

    fn build_chat_body(&self, request: &ChatRequest) -> Result<String, ChatError> {
        serialize_chat_body(self.chat_body_object(
            request.system_prompt,
            request.messages,
            request.reminders,
            request.tools_json,
        )?)
    }

    /// Like [`build_chat_body`](Self::build_chat_body) but sets `stream: true` so
    /// the provider replies with a `text/event-stream` body.
    fn build_stream_body(&self, request: &ChatRequest) -> Result<String, ChatError> {
        let mut body = self.chat_body_object(
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
        &self,
        request: &ChatJsonRequest<'_>,
        schema_name: &str,
        schema: &Value,
    ) -> Result<String, ChatError> {
        let mut body = self.chat_body_object(
            request.chat.system_prompt,
            request.chat.messages,
            request.chat.reminders,
            request.chat.tools_json,
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
    fn build_media_body(&self, request: &MediaRequest<'_>) -> Result<String, InferMediaError> {
        let Some(user_prompt) = request.user_prompt.filter(|prompt| !prompt.is_empty()) else {
            return Err(InferMediaError::IncompleteRequest);
        };
        let prepared = prepare_asset(request.media, self.context.image_max_bytes)?;
        let image_url = prepared.openai_url();

        let mut body = self.context.request_body();
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
            .map_err(|_| ClawApiError::ApiError("out of memory serializing media request").into())
    }

    pub(super) fn new(config: ClawApiConfig) -> Self {
        Self {
            context: BackendContext::new(config, CHAT_PATH, |api_key| {
                vec![("Authorization".to_string(), format!("Bearer {api_key}"))]
            }),
        }
    }

    pub(super) fn timeout_ms(&self) -> u32 {
        self.context.timeout_ms
    }

    pub(super) async fn chat<S: TcpConnect + Dns>(
        &self,
        http: &mut NetClient<'_, S>,
        request: &ChatRequest<'_>,
    ) -> Result<LlmResponse, ChatError> {
        let body = self.build_chat_body(request)?;
        let response = post_json(http, &self.context, &body).await?;
        parse_openai_chat_response(&response.body).map_err(Into::into)
    }

    pub(super) async fn chat_json<S: TcpConnect + Dns>(
        &self,
        http: &mut NetClient<'_, S>,
        request: &ChatJsonRequest<'_>,
        schema_name: &str,
        schema: &Value,
    ) -> Result<LlmResponse, ChatError> {
        let body = self.build_chat_json_body(request, schema_name, schema)?;
        let response = post_json(http, &self.context, &body).await?;
        parse_openai_chat_response(&response.body).map_err(Into::into)
    }

    pub(super) async fn infer_media<S: TcpConnect + Dns>(
        &self,
        http: &mut NetClient<'_, S>,
        request: &MediaRequest<'_>,
    ) -> Result<String, InferMediaError> {
        let body = self.build_media_body(request)?;
        let response = post_json(http, &self.context, &body).await?;
        media_text(parse_openai_chat_response(&response.body)?)
    }

    pub(super) async fn chat_stream<'h, 'r, S: TcpConnect + Dns>(
        &self,
        http: &'h mut NetClient<'_, S>,
        request: &'r ChatRequest<'r>,
    ) -> Result<ProviderStream<ResponseStream<'h>>, ChatError> {
        let body = self.build_stream_body(request)?;
        post_stream(
            http,
            &self.context,
            body,
            ProviderSse::OpenAi(OpenAiSse::new()),
        )
        .await
    }
}

#[cfg(all(test, feature = "cache_profile"))]
mod tests {
    use super::*;
    use crate::BackendKind;

    #[test]
    fn streaming_requests_ask_provider_to_include_usage() {
        let backend = OpenAiCompatible::new(ClawApiConfig::new(
            BackendKind::OpenAiCompatible,
            "key",
            "model",
            "https://example.invalid/v1",
        ));
        let messages = [];
        let body = backend
            .build_stream_body(&ChatRequest::new("system", &messages))
            .unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();

        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }
}

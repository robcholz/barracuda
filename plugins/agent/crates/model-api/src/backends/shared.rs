//! Helpers shared by the LLM backends.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embedded_nal_async::{Dns, TcpConnect};
use reqwless::response::Status;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::super::chat_stream::{drain_body, ProviderStream};
use super::super::errors::{ChatError, InferMediaError, ModelApiError};
use super::super::transport::{HttpTransport as NetClient, Response, ResponsePart, ResponseStream};
#[cfg(feature = "cache_profile")]
use super::super::types::ProviderUsage;
use super::super::types::{LlmResponse, ModelApiConfig, ToolCall};
use super::super::StatusCode;
use super::sse::ProviderSse;

/// HTTP statuses that indicate a transient, retryable server condition.
const STATUS_REQUEST_TIMEOUT: u16 = 408;
const MAX_ERROR_BODY_BYTES: usize = 1024;

#[derive(Debug)]
pub(super) struct BackendContext {
    model: String,
    endpoint: String,
    headers: Vec<(String, String)>,
    pub(super) timeout_ms: u32,
    max_tokens: u32,
    pub(super) image_max_bytes: usize,
}

impl BackendContext {
    pub(super) fn new(
        config: ModelApiConfig,
        path: &str,
        make_headers: impl FnOnce(String) -> Vec<(String, String)>,
    ) -> Self {
        let ModelApiConfig {
            api_key,
            model,
            base_url,
            timeout_ms,
            max_tokens,
            image_max_bytes,
            ..
        } = config;
        Self {
            model,
            endpoint: join_url(&base_url, path),
            headers: make_headers(api_key),
            timeout_ms,
            max_tokens,
            image_max_bytes,
        }
    }

    pub(super) fn request_body(&self) -> Map<String, Value> {
        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(self.model.clone()));
        body.insert("max_tokens".to_string(), Value::from(self.max_tokens));
        body
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
}

fn map_status_error(status: StatusCode, body: String) -> ModelApiError {
    let body = truncated_error_body(body);
    if status_is_transient(status) {
        ModelApiError::TransientHttpStatus { status, body }
    } else {
        ModelApiError::HttpStatus { status, body }
    }
}

fn truncated_error_body(mut body: String) -> String {
    if body.len() <= MAX_ERROR_BODY_BYTES {
        return body;
    }
    let mut end = MAX_ERROR_BODY_BYTES;
    while !body.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    body.truncate(end);
    body.push('…');
    body
}

fn status_is_transient(status: StatusCode) -> bool {
    let code = status.0;
    code == STATUS_REQUEST_TIMEOUT || status == Status::TooManyRequests || status.is_server_error()
}

pub(super) async fn post_json<Tcp: TcpConnect, Resolver: Dns>(
    http: &mut NetClient<'_, Tcp, Resolver>,
    context: &BackendContext,
    body: &str,
) -> Result<Response, ModelApiError> {
    let header_refs = context
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let response = http
        .post_json(context.endpoint(), body, &header_refs)
        .await?;
    if response.status.is_successful() {
        Ok(response)
    } else {
        Err(map_status_error(response.status, response.body))
    }
}

pub(super) async fn post_stream<'h, Tcp: TcpConnect, Resolver: Dns>(
    http: &'h mut NetClient<'_, Tcp, Resolver>,
    context: &BackendContext,
    body: String,
    sse: ProviderSse,
) -> Result<ProviderStream<ResponseStream<'h>>, ChatError> {
    let mut stream = http.post_json_stream(
        context.endpoint().to_string(),
        body,
        context.headers().to_vec(),
    );
    let status = match futures_lite::StreamExt::next(&mut stream).await {
        Some(Ok(ResponsePart::Head(status))) => status,
        Some(Ok(ResponsePart::Data(_))) | None => {
            return Err(ModelApiError::ApiError("HTTP stream ended before response head").into());
        }
        Some(Err(error)) => return Err(ModelApiError::from(error).into()),
    };
    if !status.is_successful() {
        let body = drain_body(stream).await.map_err(ModelApiError::from)?;
        return Err(map_status_error(status, body).into());
    }
    Ok(ProviderStream::new(stream, sse))
}

/// Extract the required non-empty assistant text from a media inference reply.
pub(super) fn media_text(parsed: LlmResponse) -> Result<String, InferMediaError> {
    match parsed.text {
        Some(text) if !text.is_empty() => Ok(text),
        _ => Err(ModelApiError::EmptyResponse.into()),
    }
}

/// Join `base_url` and `path` with exactly one slash between them.
fn join_url(base_url: &str, path: &str) -> String {
    let base_has_slash = base_url.ends_with('/');
    let path_has_slash = path.starts_with('/');
    if base_has_slash && path_has_slash {
        format!("{base_url}{}", &path[1..])
    } else if !base_has_slash && !path_has_slash {
        format!("{base_url}/{path}")
    } else {
        format!("{base_url}{path}")
    }
}

#[derive(Deserialize, Serialize)]
struct OpenAiResponse {
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[cfg(feature = "cache_profile")]
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize, Serialize)]
struct OpenAiChoice {
    message: Option<OpenAiMessage>,
}

#[derive(Deserialize, Serialize)]
struct OpenAiMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OpenAiToolCall>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct OpenAiToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function: Option<OpenAiFunctionCall>,
}

#[derive(Deserialize, Serialize)]
struct OpenAiFunctionCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
}

#[cfg(feature = "cache_profile")]
#[derive(Deserialize, Serialize)]
pub(super) struct OpenAiUsage {
    prompt_tokens: Option<u64>,
    input_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    output_tokens: Option<u64>,
    prompt_tokens_details: Option<TokenDetails>,
    input_tokens_details: Option<TokenDetails>,
    cache_write_tokens: Option<u64>,
}

#[cfg(feature = "cache_profile")]
#[derive(Deserialize, Serialize)]
struct TokenDetails {
    cached_tokens: Option<u64>,
}

#[cfg(feature = "cache_profile")]
impl OpenAiUsage {
    pub(super) fn profile(self) -> Option<ProviderUsage> {
        let profile = ProviderUsage {
            input_tokens: self.prompt_tokens.or(self.input_tokens),
            output_tokens: self.completion_tokens.or(self.output_tokens),
            cache_read_tokens: self
                .prompt_tokens_details
                .and_then(|details| details.cached_tokens)
                .or_else(|| {
                    self.input_tokens_details
                        .and_then(|details| details.cached_tokens)
                }),
            cache_write_tokens: self.cache_write_tokens,
        };
        (profile.input_tokens.is_some()
            || profile.output_tokens.is_some()
            || profile.cache_read_tokens.is_some()
            || profile.cache_write_tokens.is_some())
        .then_some(profile)
    }
}

#[cfg(feature = "cache_profile")]
#[derive(Deserialize, Serialize)]
pub(super) struct AnthropicUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

#[cfg(feature = "cache_profile")]
impl AnthropicUsage {
    pub(super) fn profile(self) -> Option<ProviderUsage> {
        let profile = ProviderUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_input_tokens,
            cache_write_tokens: self.cache_creation_input_tokens,
        };
        (profile.input_tokens.is_some()
            || profile.output_tokens.is_some()
            || profile.cache_read_tokens.is_some()
            || profile.cache_write_tokens.is_some())
        .then_some(profile)
    }
}

/// Parse an OpenAI chat-completions response.
pub(super) fn parse_openai_chat_response(body: &str) -> Result<LlmResponse, ModelApiError> {
    let mut response: OpenAiResponse =
        serde_json::from_str(body).map_err(|_| ModelApiError::Parse)?;
    let message = response
        .choices
        .first_mut()
        .and_then(|choice| choice.message.take())
        .ok_or(ModelApiError::MalformedResponse("response missing message"))?;

    if message.role.as_deref() != Some("assistant") {
        return Err(ModelApiError::MalformedResponse(
            "response message is not assistant",
        ));
    }

    let raw_message_json = serde_json::to_string(&message)
        .map_err(|_| ModelApiError::ApiError("out of memory copying raw message"))?;

    let text = message.content.filter(|content| !content.is_empty());
    let reasoning_content = message.reasoning_content;

    let tool_calls = message
        .tool_calls
        .into_iter()
        .map(|call| -> Result<_, ModelApiError> {
            let function = call
                .function
                .ok_or(ModelApiError::MalformedResponse("malformed tool call"))?;
            Ok(ToolCall {
                id: call
                    .id
                    .ok_or(ModelApiError::MalformedResponse("malformed tool call"))?,
                name: function
                    .name
                    .ok_or(ModelApiError::MalformedResponse("malformed tool call"))?,
                arguments_json: function
                    .arguments
                    .ok_or(ModelApiError::MalformedResponse("malformed tool call"))?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    if text.is_none() && tool_calls.is_empty() {
        return Err(ModelApiError::EmptyResponse);
    }

    Ok(LlmResponse {
        text,
        reasoning_content,
        raw_message_json: Some(raw_message_json),
        tool_calls,
        #[cfg(feature = "cache_profile")]
        usage: response.usage.and_then(OpenAiUsage::profile),
    })
}

/// Insert OpenAI-style `tools` into a chat request body map.
pub(super) fn insert_tools_into_body(
    body: &mut Map<String, Value>,
    tools_json: &str,
) -> Result<(), ChatError> {
    let tools: Value = serde_json::from_str(tools_json).map_err(|_| ChatError::InvalidToolsJson)?;
    if !tools.is_array() {
        return Err(ChatError::InvalidToolsJson);
    }
    body.insert("tools".to_string(), tools);
    Ok(())
}

pub(super) fn serialize_chat_body(body: Map<String, Value>) -> Result<String, ChatError> {
    serde_json::to_string(&Value::Object(body))
        .map_err(|_| ModelApiError::ApiError("out of memory serializing request").into())
}

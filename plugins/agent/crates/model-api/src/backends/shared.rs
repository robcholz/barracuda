//! Helpers shared by the LLM backends.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_bulk_memory::BulkVec;
use embedded_nal_async::{Dns, TcpConnect};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::super::chat_stream::{drain_body, ProviderStream};
use super::super::errors::Error;
use super::super::transport::{Response, ResponsePart, Transport};
#[cfg(feature = "cache_profile")]
use super::super::types::ProviderUsage;
use super::super::types::{LlmResponse, ToolCall};
use super::sse::ProviderSse;
use super::Backend;

/// HTTP statuses that indicate a transient, retryable server condition.
const STATUS_REQUEST_TIMEOUT: u16 = 408;
const MAX_ERROR_BODY_BYTES: usize = 1024;

fn map_status_error(status: u16, body: String) -> Error {
    let body = truncated_error_body(body);
    if status_is_transient(status) {
        Error::TransientHttpStatus { status, body }
    } else {
        Error::HttpStatus { status, body }
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

fn status_is_transient(status: u16) -> bool {
    status == STATUS_REQUEST_TIMEOUT || status == 429 || (500..600).contains(&status)
}

pub(super) async fn post_json(
    http: &mut Transport<'_, impl TcpConnect, impl Dns>,
    backend: &Backend,
    body: &[u8],
) -> Result<Response, Error> {
    let header_refs = backend
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let response = http
        .post_json(&backend.endpoint, body, &header_refs)
        .await?;
    if (200..300).contains(&response.status) {
        Ok(response)
    } else {
        Err(map_status_error(response.status, response.body))
    }
}

pub(super) async fn post_stream<'h>(
    http: &'h mut Transport<'_, impl TcpConnect, impl Dns>,
    backend: &Backend,
    body: BulkVec<u8>,
    sse: ProviderSse,
) -> Result<ProviderStream<'h>, Error> {
    let mut stream = http.post_json_stream(backend.endpoint.clone(), body, backend.headers.clone());
    let status = match futures_lite::StreamExt::next(&mut stream).await {
        Some(Ok(ResponsePart::Head(status))) => status,
        Some(Ok(ResponsePart::Data(_))) | None => {
            return Err(Error::Api("HTTP stream ended before response head"));
        }
        Some(Err(error)) => return Err(error),
    };
    if !(200..300).contains(&status) {
        let body = drain_body(stream).await?;
        return Err(map_status_error(status, body));
    }
    Ok(ProviderStream::new(stream, sse))
}

/// Extract the required non-empty assistant text from a media inference reply.
pub(super) fn media_text(parsed: LlmResponse) -> Result<String, Error> {
    match parsed.text {
        Some(text) if !text.is_empty() => Ok(text),
        _ => Err(Error::EmptyResponse),
    }
}

/// Join `base_url` and `path` with exactly one slash between them.
pub(super) fn join_url(base_url: &str, path: &str) -> String {
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
pub(super) fn parse_openai_chat_response(body: &str) -> Result<LlmResponse, Error> {
    let mut response: OpenAiResponse = serde_json::from_str(body).map_err(|_| Error::Parse)?;
    let message = response
        .choices
        .first_mut()
        .and_then(|choice| choice.message.take())
        .ok_or(Error::MalformedResponse("response missing message"))?;

    if message.role.as_deref() != Some("assistant") {
        return Err(Error::MalformedResponse(
            "response message is not assistant",
        ));
    }

    let raw_message_json = serde_json::to_string(&message)
        .map_err(|_| Error::Api("out of memory copying raw message"))?;

    let text = message.content.filter(|content| !content.is_empty());
    let reasoning_content = message.reasoning_content;

    let tool_calls = message
        .tool_calls
        .into_iter()
        .map(|call| -> Result<_, Error> {
            let function = call
                .function
                .ok_or(Error::MalformedResponse("malformed tool call"))?;
            Ok(ToolCall {
                id: call
                    .id
                    .ok_or(Error::MalformedResponse("malformed tool call"))?,
                name: function
                    .name
                    .ok_or(Error::MalformedResponse("malformed tool call"))?,
                arguments_json: function
                    .arguments
                    .ok_or(Error::MalformedResponse("malformed tool call"))?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    if text.is_none() && tool_calls.is_empty() {
        return Err(Error::EmptyResponse);
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

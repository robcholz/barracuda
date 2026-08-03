//! Helpers shared by the LLM backends.

use core::sync::atomic::AtomicBool;

use claw_interface::http::{
    blocking::ClawHttp as BlockingClawHttp, Cancel, ClawHttp, HttpAuth, HttpError, HttpHeader,
    HttpJsonRequest, HttpResponse, HttpStatusCode, StreamingHttp,
};
use serde_json::{Map, Value};

use super::super::chat_stream::{drain_body, ProviderStream};
use super::super::errors::{ChatError, ClawApiError, InferMediaError};
#[cfg(feature = "cache_profile")]
use super::super::types::ProviderUsage;
use super::super::types::{ClawApiConfig, LlmResponse, MediaAsset, ToolCall};
use super::sse::ProviderSse;

/// HTTP statuses that indicate a transient, retryable server condition.
const STATUS_REQUEST_TIMEOUT: u16 = 408;
const STATUS_TOO_MANY_REQUESTS: u16 = 429;
const STATUS_SERVER_ERROR_MIN: u16 = 500;
const STATUS_SERVER_ERROR_MAX: u16 = 599;

#[derive(Clone, Debug)]
pub(super) struct BackendContext {
    api_key: String,
    model: String,
    base_url: String,
    timeout_ms: u32,
    max_tokens: u32,
    image_max_bytes: usize,
}

impl BackendContext {
    pub(super) fn from_config(config: &ClawApiConfig) -> Self {
        Self {
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            timeout_ms: config.timeout_ms,
            max_tokens: config.max_tokens,
            image_max_bytes: config.image_max_bytes,
        }
    }

    pub(super) fn api_key(&self) -> &str {
        &self.api_key
    }

    pub(super) fn model(&self) -> &str {
        &self.model
    }

    pub(super) fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    pub(super) fn image_max_bytes(&self) -> usize {
        self.image_max_bytes
    }

    pub(super) fn endpoint_url(&self, chat_path: &str) -> String {
        join_url(&self.base_url, chat_path)
    }

    /// Assemble the owned inputs for a single JSON POST to `path`.
    ///
    /// This is the transport-agnostic half of a request: the backend builds the
    /// serialized `body` and chooses `auth`/`headers`, and the blocking and async
    /// send paths then borrow the same [`PreparedRequest`] instead of each
    /// re-deriving the URL, auth, and headers.
    pub(super) fn prepare(
        &self,
        path: &str,
        body: String,
        auth: PreparedAuth,
        headers: Vec<(&'static str, String)>,
    ) -> PreparedRequest {
        PreparedRequest {
            url: self.endpoint_url(path),
            body,
            timeout_ms: self.timeout_ms,
            auth,
            headers,
        }
    }
}

/// Owned authentication for a [`PreparedRequest`], mapped to a borrowing
/// [`HttpAuth`] only at send time.
pub(super) enum PreparedAuth {
    None,
    Bearer(String),
}

/// A fully assembled but not-yet-sent JSON POST.
///
/// Owning the URL, body, auth, and headers lets the blocking and async send
/// helpers share one assembly path; the only remaining difference between them
/// is the actual `post_json` call.
pub(super) struct PreparedRequest {
    url: String,
    body: String,
    timeout_ms: u32,
    auth: PreparedAuth,
    headers: Vec<(&'static str, String)>,
}

impl PreparedRequest {
    fn header_refs(&self) -> Vec<HttpHeader<'_>> {
        self.headers
            .iter()
            .map(|(name, value)| HttpHeader { name, value })
            .collect()
    }

    fn auth(&self) -> HttpAuth<'_> {
        match &self.auth {
            PreparedAuth::None => HttpAuth::None,
            PreparedAuth::Bearer(key) => HttpAuth::Bearer(key),
        }
    }

    fn request<'a>(&'a self, headers: &'a [HttpHeader<'a>]) -> HttpJsonRequest<'a> {
        HttpJsonRequest {
            url: &self.url,
            body: &self.body,
            auth: self.auth(),
            timeout_ms: self.timeout_ms,
            headers,
        }
    }
}

/// Map a transport [`HttpError`] to a [`ClawApiError`], classifying whether the
/// failure is transient (retryable) or permanent. The retry decision is made by
/// the [`crate::ClawApi`] retry loop via [`ClawApiError::is_retryable`].
pub(crate) fn map_http_error(err: HttpError) -> ClawApiError {
    if is_transient(&err) {
        ClawApiError::TransientTransport(err)
    } else {
        ClawApiError::Transport(err)
    }
}

fn is_transient(err: &HttpError) -> bool {
    match err {
        HttpError::Aborted | HttpError::InvalidUrl | HttpError::InvalidBody => false,
        HttpError::ClientInitFailed | HttpError::RequestFailed(_) => true,
        HttpError::UnexpectedStatus { status, .. } => status_is_transient(*status),
    }
}

fn status_is_transient(status: HttpStatusCode) -> bool {
    let code = status.as_u16();
    code == STATUS_REQUEST_TIMEOUT
        || code == STATUS_TOO_MANY_REQUESTS
        || (STATUS_SERVER_ERROR_MIN..=STATUS_SERVER_ERROR_MAX).contains(&code)
}

pub(super) fn post_json<H: BlockingClawHttp>(
    http: &mut H,
    request: &HttpJsonRequest<'_>,
    abort: &AtomicBool,
) -> Result<HttpResponse, ClawApiError> {
    http.post_json(request, abort).map_err(map_http_error)
}

pub(super) async fn post_json_async<'a, H: ClawHttp>(
    http: &'a mut H,
    request: &'a HttpJsonRequest<'a>,
    cancel: Cancel<'a>,
) -> Result<HttpResponse, ClawApiError> {
    http.post_json(request, cancel)
        .await
        .map_err(map_http_error)
}

/// Send a [`PreparedRequest`] over the blocking transport.
pub(super) fn post_prepared<H: BlockingClawHttp>(
    http: &mut H,
    prepared: &PreparedRequest,
    abort: &AtomicBool,
) -> Result<HttpResponse, ClawApiError> {
    let headers = prepared.header_refs();
    let request = prepared.request(&headers);
    post_json(http, &request, abort)
}

/// Send a [`PreparedRequest`] over the async transport.
pub(super) async fn post_prepared_async<H: ClawHttp>(
    http: &mut H,
    prepared: &PreparedRequest,
    cancel: Cancel<'_>,
) -> Result<HttpResponse, ClawApiError> {
    let headers = prepared.header_refs();
    let request = prepared.request(&headers);
    post_json_async(http, &request, cancel).await
}

/// Open a streaming chat completion from a [`PreparedRequest`], wrapping a 2xx
/// body stream in `sse` and surfacing a non-2xx status as a drained error body.
pub(super) async fn post_prepared_stream<'h, H: StreamingHttp>(
    http: &'h mut H,
    prepared: &PreparedRequest,
    cancel: Cancel<'h>,
    sse: ProviderSse,
) -> Result<ProviderStream<H::ByteStream<'h>>, ChatError> {
    let headers = prepared.header_refs();
    let request = prepared.request(&headers);
    let (status, stream) = http
        .post_json_streaming(&request, cancel)
        .await
        .map_err(map_http_error)?;
    if !status.is_success() {
        let body = drain_body(stream).await.map_err(map_http_error)?;
        return Err(map_http_error(HttpError::UnexpectedStatus {
            status,
            message: format!("HTTP {status}: {body}"),
        })
        .into());
    }
    Ok(ProviderStream::new(stream, sse))
}

/// Extract the required non-empty assistant text from a media inference reply.
pub(super) fn media_text(parsed: LlmResponse) -> Result<String, InferMediaError> {
    match parsed.text {
        Some(text) if !text.is_empty() => Ok(text),
        _ => Err(ClawApiError::EmptyResponse.into()),
    }
}

/// `join_url` from the backends: join `base_url` and `path` with exactly one
/// slash between them.
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

/// Parse an OpenAI chat-completions response, mirroring `parse_chat_response`
/// in `claw_llm_backend_openai_compatible.c`.
pub(super) fn parse_openai_chat_response(body: &str) -> Result<LlmResponse, ClawApiError> {
    let root: Value = serde_json::from_str(body).map_err(|_| ClawApiError::Parse)?;

    let message = root
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c0| c0.get("message"));
    let message = match message {
        Some(m) if m.is_object() => m,
        _ => return Err(ClawApiError::MalformedResponse("response missing message")),
    };

    if message.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return Err(ClawApiError::MalformedResponse(
            "response message is not assistant",
        ));
    }

    let raw_message_json = serde_json::to_string(message)
        .map_err(|_| ClawApiError::ApiError("out of memory copying raw message"))?;

    let text = message
        .get("content")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // reasoning_content: kept even when empty, as long as it is a string.
    let reasoning_content = message
        .get("reasoning_content")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string());

    let mut tool_calls = Vec::new();
    if let Some(arr) = message.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in arr {
            let function = tc.get("function");
            let id = tc.get("id");
            let name = function.and_then(|f| f.get("name"));
            let args = function.and_then(|f| f.get("arguments"));
            match (
                id.and_then(Value::as_str),
                name.and_then(Value::as_str),
                args.and_then(Value::as_str),
            ) {
                (Some(id), Some(name), Some(args)) => tool_calls.push(ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    arguments_json: args.to_string(),
                }),
                _ => return Err(ClawApiError::MalformedResponse("malformed tool call")),
            }
        }
    }

    if text.is_none() && tool_calls.is_empty() {
        return Err(ClawApiError::EmptyResponse);
    }

    Ok(LlmResponse {
        text,
        reasoning_content,
        raw_message_json: Some(raw_message_json),
        tool_calls,
        #[cfg(feature = "cache_profile")]
        usage: parse_openai_usage(&root),
    })
}

/// Extract OpenAI-compatible usage counters for cache profiling.
#[cfg(feature = "cache_profile")]
pub(super) fn parse_openai_usage(root: &Value) -> Option<ProviderUsage> {
    let usage = root.get("usage")?;
    let prompt_details = usage.get("prompt_tokens_details");
    let input_details = usage.get("input_tokens_details");
    let profile = ProviderUsage {
        input_tokens: usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(Value::as_u64),
        output_tokens: usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(Value::as_u64),
        cache_read_tokens: prompt_details
            .and_then(|details| details.get("cached_tokens"))
            .or_else(|| input_details.and_then(|details| details.get("cached_tokens")))
            .and_then(Value::as_u64),
        cache_write_tokens: usage.get("cache_write_tokens").and_then(Value::as_u64),
    };
    (profile.input_tokens.is_some()
        || profile.output_tokens.is_some()
        || profile.cache_read_tokens.is_some()
        || profile.cache_write_tokens.is_some())
    .then_some(profile)
}

/// Extract Anthropic usage counters for cache profiling.
#[cfg(feature = "cache_profile")]
pub(super) fn parse_anthropic_usage(root: &Value) -> Option<ProviderUsage> {
    let usage = root
        .get("usage")
        .or_else(|| root.get("message").and_then(|message| message.get("usage")))?;
    let profile = ProviderUsage {
        input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
        output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
        cache_read_tokens: usage.get("cache_read_input_tokens").and_then(Value::as_u64),
        cache_write_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64),
    };
    (profile.input_tokens.is_some()
        || profile.output_tokens.is_some()
        || profile.cache_read_tokens.is_some()
        || profile.cache_write_tokens.is_some())
    .then_some(profile)
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

/// Select the single media asset a backend will send.
///
/// An empty asset list is a returnable [`InferMediaError::IncompleteRequest`].
/// Sending more than one asset in a single request is rejected rather than
/// silently dropping the extra assets.
pub(super) fn single_media_asset(media: &[MediaAsset]) -> Result<&MediaAsset, InferMediaError> {
    match media {
        [] => Err(InferMediaError::IncompleteRequest),
        [asset] => Ok(asset),
        _ => Err(InferMediaError::MultipleMediaAssetsUnsupported),
    }
}

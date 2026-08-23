//! `barracuda-model-api` error types.
//!
//! Each public entry point ([`crate::ModelApi::set_config`], [`crate::ModelApi::chat`],
//! [`crate::ModelApi::infer_media`]) returns its own error enum because their
//! failure modes are not 1-to-1: config validation, chat-only tool errors, and
//! the media pipeline are disjoint. The shared API/transport/parse failures live
//! in [`ModelApiError`], which the per-function enums wrap via `#[from]`.
//!
//! Transport variants retain the typed [`crate::HttpError`] source; other dynamic
//! messages are limited to the variants whose domain data is itself text.

use alloc::string::String;
use strum::IntoStaticStr;
use thiserror::Error;

use crate::{transport::Error as NetError, StatusCode};

const TRUNCATED_STREAM_MESSAGE: &str = "stream ended before provider completion";

/// Failures shared by chat and media calls (transport, response parsing,
/// allocation). `ApiError` is the static-message catch-all.
#[derive(Debug, IntoStaticStr, Error)]
pub enum ModelApiError {
    /// The client was constructed but no valid config has been installed yet.
    #[error("LLM API is not configured")]
    #[strum(serialize = "not_configured")]
    NotConfigured,
    /// Transport failure. The typed HTTP failure is retained as the error
    /// source and decides its own retryability.
    #[error("HTTP transport error: {0}")]
    #[strum(serialize = "transport")]
    Transport(#[from] NetError),
    /// The configured request deadline elapsed.
    #[error("LLM HTTP request timed out")]
    #[strum(serialize = "timeout")]
    Timeout,
    /// Permanent non-success HTTP response.
    #[error("HTTP {}: {body}", status.0)]
    #[strum(serialize = "http_status")]
    HttpStatus { status: StatusCode, body: String },
    /// Retryable non-success HTTP response (408, 429, or 5xx).
    #[error("transient HTTP {}: {body}", status.0)]
    #[strum(serialize = "transient_http_status")]
    TransientHttpStatus { status: StatusCode, body: String },
    /// The response body was not valid JSON.
    #[error("failed to parse LLM JSON response")]
    #[strum(serialize = "parse")]
    Parse,
    /// The model returned no usable content.
    #[error("LLM returned an empty response")]
    #[strum(serialize = "empty_response")]
    EmptyResponse,
    /// The response JSON had an unexpected shape (missing/!assistant message,
    /// missing content, malformed tool call).
    #[error("malformed LLM response: {0}")]
    #[strum(serialize = "malformed_response")]
    MalformedResponse(&'static str),
    /// Any other API-side failure (allocation, serialization, ...).
    #[error("{0}")]
    #[strum(serialize = "api")]
    ApiError(&'static str),
}

impl ModelApiError {
    /// Whether retrying the same request might succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ModelApiError::TransientHttpStatus { .. } | ModelApiError::Timeout
        ) || matches!(self, ModelApiError::Transport(error) if error.retryable())
    }

    /// Whether this failure came from aborting an in-flight request.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        matches!(self, ModelApiError::Transport(NetError::Cancelled))
    }
}

/// Failures from constructing a [`crate::ModelApi`] (config validation + backend
/// selection).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InitError {
    #[error("LLM API key is empty")]
    MissingApiKey,
    #[error("LLM model is empty")]
    MissingModel,
    #[error("LLM base URL is empty")]
    MissingBaseUrl,
}

/// Failures from a structured JSON chat completion request.
#[derive(Debug, Error)]
pub enum ChatJsonError {
    /// Model returned neither parseable JSON nor tool calls.
    #[error("LLM returned empty structured output")]
    EmptyText,
    /// Parsed text was not valid JSON for the expected output type.
    #[error("invalid structured output: {0}")]
    InvalidOutput(String),
    /// [`crate::ModelApi::chat_json`] was called without
    /// [`crate::ChatJsonRequest::with_output_schema`].
    #[error("structured chat requires an output schema")]
    MissingOutputSchema,
    /// A shared chat completion failure.
    #[error(transparent)]
    Chat(#[from] ChatError),
}

impl ChatJsonError {
    /// Retryable only when the underlying chat transport failure is transient;
    /// schema/parse failures are deterministic and never retried.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, ChatJsonError::Chat(err) if err.is_retryable())
    }
}

/// Failures from [`crate::ModelApi::chat`].
///
/// Transient transport failures are retried automatically per the request's
/// [`RetryPolicy`](crate::RetryPolicy); a `ChatError` therefore represents a
/// final failure. Use [`ChatError::is_retryable`] to decide whether retrying
/// the whole operation (e.g. after rebuilding the request) is worthwhile.
///
/// ```
/// use barracuda_model_api::{ChatError, ModelApiError};
/// fn handle(err: &ChatError) {
///     match err {
///         ChatError::Api(ModelApiError::Transport(http)) if http.retryable() => {
///             eprintln!("transient, may retry: {http}");
///         }
///         other => eprintln!("permanent failure: {other}"),
///     }
/// }
/// ```
#[derive(Debug, Error)]
pub enum ChatError {
    /// The caller-supplied tools JSON was invalid.
    #[error("invalid tools JSON")]
    InvalidToolsJson,
    /// A shared API/transport/parse failure.
    #[error(transparent)]
    Api(#[from] ModelApiError),
}

impl ChatError {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::InvalidToolsJson => "invalid_tools_json",
            Self::Api(error) => error.into(),
        }
    }

    /// A streaming response that ended before the provider's terminal marker.
    #[must_use]
    pub fn truncated_stream() -> Self {
        ChatError::Api(ModelApiError::MalformedResponse(TRUNCATED_STREAM_MESSAGE))
    }

    /// Whether retrying the same request might succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ChatError::Api(ModelApiError::MalformedResponse(message))
                if *message == TRUNCATED_STREAM_MESSAGE
        ) || matches!(self, ChatError::Api(err) if err.is_retryable())
    }

    /// Whether this chat request was aborted by the caller.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        matches!(self, ChatError::Api(err) if err.is_aborted())
    }
}

/// Failures from a one-shot media inference request (includes the media-prep
/// pipeline used only by this call).
#[derive(Debug, Error)]
pub enum InferMediaError {
    /// The request was missing a prompt or media asset.
    #[error("media request is incomplete")]
    IncompleteRequest,
    /// Media URL was empty.
    #[error("media URL is empty")]
    MediaUrlEmpty,
    /// The media file was empty.
    #[error("media file is empty")]
    MediaFileEmpty,
    /// The media file exceeded the configured size limit.
    #[error("media file is too large")]
    MediaTooLarge,
    /// The backend requires local image data (e.g. Anthropic base64).
    #[error("backend requires local image data")]
    RequiresLocalImage,
    /// A shared API/transport/parse failure.
    #[error(transparent)]
    Api(#[from] ModelApiError),
}

impl InferMediaError {
    /// Whether retrying the same request might succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, InferMediaError::Api(err) if err.is_retryable())
    }
}

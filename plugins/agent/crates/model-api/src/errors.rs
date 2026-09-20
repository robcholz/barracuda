//! Errors returned by the model client.

use alloc::string::String;

use strum::IntoStaticStr;
use thiserror::Error as ThisError;

const TRUNCATED_STREAM_MESSAGE: &str = "stream ended before provider completion";

/// A model request failure.
#[derive(Debug, IntoStaticStr, ThisError)]
pub enum Error {
    #[error("LLM API is not configured")]
    #[strum(serialize = "not_configured")]
    NotConfigured,
    #[error("request was cancelled")]
    #[strum(serialize = "cancelled")]
    Cancelled,
    #[error("HTTPS requested without a TLS configuration")]
    #[strum(serialize = "tls_not_configured")]
    TlsNotConfigured,
    #[error("invalid URL")]
    #[strum(serialize = "invalid_url")]
    InvalidUrl,
    #[error("invalid HTTP header")]
    #[strum(serialize = "invalid_header")]
    InvalidHeader,
    #[error("HTTP codec error")]
    #[strum(serialize = "http_codec")]
    HttpCodec,
    #[error("HTTP scratch memory allocation failed")]
    #[strum(serialize = "allocation")]
    Allocation,
    #[error("connection was aborted")]
    #[strum(serialize = "connection_aborted")]
    ConnectionAborted,
    #[error("response body is not UTF-8")]
    #[strum(serialize = "invalid_utf8")]
    InvalidUtf8,
    #[error(transparent)]
    #[strum(serialize = "http")]
    Http(#[from] reqwless::Error),
    #[error("LLM HTTP request timed out")]
    #[strum(serialize = "timeout")]
    Timeout,
    #[error("HTTP {status}: {body}")]
    #[strum(serialize = "http_status")]
    HttpStatus { status: u16, body: String },
    #[error("transient HTTP {status}: {body}")]
    #[strum(serialize = "transient_http_status")]
    TransientHttpStatus { status: u16, body: String },
    #[error("failed to parse LLM JSON response")]
    #[strum(serialize = "parse")]
    Parse,
    #[error("LLM returned an empty response")]
    #[strum(serialize = "empty_response")]
    EmptyResponse,
    #[error("malformed LLM response: {0}")]
    #[strum(serialize = "malformed_response")]
    MalformedResponse(&'static str),
    #[error("{0}")]
    #[strum(serialize = "api")]
    Api(&'static str),
    #[error("invalid tools JSON")]
    #[strum(serialize = "invalid_tools_json")]
    InvalidToolsJson,
    #[error("LLM returned empty structured output")]
    #[strum(serialize = "empty_structured_output")]
    EmptyStructuredOutput,
    #[error("invalid structured output: {0}")]
    #[strum(serialize = "invalid_structured_output")]
    InvalidStructuredOutput(String),
    #[error("structured chat requires an output schema")]
    #[strum(serialize = "missing_output_schema")]
    MissingOutputSchema,
    #[error("media request is incomplete")]
    #[strum(serialize = "incomplete_media_request")]
    IncompleteMediaRequest,
    #[error("media URL is empty")]
    #[strum(serialize = "media_url_empty")]
    MediaUrlEmpty,
    #[error("media file is empty")]
    #[strum(serialize = "media_file_empty")]
    MediaFileEmpty,
    #[error("media file is too large")]
    #[strum(serialize = "media_too_large")]
    MediaTooLarge,
    #[error("backend requires local image data")]
    #[strum(serialize = "requires_local_image")]
    RequiresLocalImage,
}

impl Error {
    pub(crate) fn kind(&self) -> &'static str {
        self.into()
    }

    pub(crate) fn truncated_stream() -> Self {
        Self::MalformedResponse(TRUNCATED_STREAM_MESSAGE)
    }

    /// Whether retrying the same request might succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::TransientHttpStatus { .. }
                | Self::Timeout
                | Self::ConnectionAborted
                | Self::MalformedResponse(TRUNCATED_STREAM_MESSAGE)
                | Self::Http(
                    reqwless::Error::Dns
                        | reqwless::Error::Network(_)
                        | reqwless::Error::ConnectionAborted
                )
        )
    }

    /// Whether this request was aborted by the caller.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// Invalid model API configuration.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum InitError {
    #[error("LLM API key is empty")]
    MissingApiKey,
    #[error("LLM model is empty")]
    MissingModel,
    #[error("LLM base URL is empty")]
    MissingBaseUrl,
}

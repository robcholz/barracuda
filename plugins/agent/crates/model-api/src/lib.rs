#![no_std]

//! `barracuda-model-api` — LLM client: OpenAI-/Anthropic-compatible chat, structured JSON
//! output, and image inference over an injected HTTP transport.
//!
//! The standalone LLM client is reusable independently of agent execution.
//!
//! # Overview
//!
//! [`ModelApi`] owns one long-lived shared HTTP client, including its persistent
//! connection and reusable buffers. Install a complete [`ModelApiConfig`], then
//! issue requests:
//!
//! | Method | Request | Returns |
//! |---|---|---|
//! | [`ModelApi::chat`] | [`ChatRequest`] | [`LlmResponse`] (text + tool calls) |
//! | [`ModelApi::chat_json`] | [`ChatJsonRequest`] | [`ChatJsonResponse`] (parsed `T` + tool calls) |
//! | [`ModelApi::infer_media`] | [`MediaRequest`] | `String` (model text about the image) |
//! | [`ModelApi::chat_stream`] | [`ChatRequest`] | [`ChatStream`] of [`ChatStreamEvent`] values |
//!
//! Networking is delegated to the workspace-wide [`http_client::Client`]
//! facade. [`ModelApi`] receives that client directly and has no TCP, DNS, TLS,
//! buffer, or concrete HTTP-engine parameters.
//!
//! # Cancellation
//!
//! Calls take [`barracuda_runtime_utils::Cancel`]. For streaming, that token covers send,
//! headers, and response-body reads; dropping [`ChatStream`] also cancels the
//! body. Cancellation is non-retryable.
//!
//! # Retries
//!
//! Retry is configured **per call** via [`RetryPolicy`] on the request (not on
//! the client). A freshly constructed request carries [`RetryPolicy::default`]
//! (2 retries, 500ms initial interval, exponential, capped at 8s); override it
//! with `.with_retry(...)`, or disable retry with [`RetryPolicy::none`]. Only
//! transient transport failures are retried (network errors and HTTP
//! 408/429/5xx); aborts, bad URLs/bodies, and other 4xx are never retried. See
//! [`RetryPolicy`] for the knobs and [`ModelApiError::is_retryable`] for the
//! classification.
//!
//! See `examples/client.rs` for a complete wire-level example.

#![cfg_attr(
    not(test),
    forbid(
        clippy::arithmetic_side_effects,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )
)]
#![cfg_attr(
    not(test),
    warn(clippy::todo, clippy::unimplemented, clippy::unreachable)
)]

extern crate alloc;

// Implementation modules are private: the public surface is the curated
// re-exports below. The backend registry, media-prep pipeline, prompt helpers,
// and retry loop are internal details, not part of the end-user API.
mod backends;
mod chat_stream;
mod client;
mod errors;
mod media;
mod retry;
mod transport;
mod types;

pub use backends::{BackendKind, ParseBackendKindError};
pub use barracuda_runtime_utils::stream;
pub use chat_stream::ChatStream;
pub use client::{ModelApi, ModelApiFactory};
pub use errors::{ChatError, ChatJsonError, InferMediaError, InitError, ModelApiError};
pub use transport::Error as HttpError;
#[cfg(feature = "cache_profile")]
pub use types::ProviderUsage;
pub use types::{
    ChatJsonRequest, ChatJsonResponse, ChatRequest, ChatStreamEvent, LlmResponse, MediaAsset,
    MediaRequest, ModelApiConfig, RetryPolicy, StaticOutputSchema, ToolCall,
};

/// HTTP response status used by Model API errors without exposing the
/// underlying HTTP transport implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusCode(pub u16);

impl StatusCode {
    pub(crate) fn is_successful(self) -> bool {
        (200..300).contains(&self.0)
    }

    pub(crate) fn is_server_error(self) -> bool {
        (500..600).contains(&self.0)
    }
}

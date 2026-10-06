#![no_std]
// Without atomic compare-and-swap (ESP32-C3, ESP32-S2) `tracing` compiles to
// nothing, so values only traced look unused there.
#![cfg_attr(not(target_has_atomic = "ptr"), allow(unused))]

//! `barracuda-model-api` — LLM client: OpenAI-/Anthropic-compatible chat, structured JSON
//! output, and image inference over an injected HTTP transport.
//!
//! The standalone LLM client is reusable independently of agent execution.
//!
//! # Overview
//!
//! [`ModelApi`] owns one long-lived Agent HTTP client, including its persistent
//! connection and reusable buffers. Install a complete [`ModelApiConfig`], then
//! issue requests:
//!
//! | Method | Request | Returns |
//! |---|---|---|
//! | [`ModelApi::chat`] | [`ChatRequest`] | [`LlmResponse`] (text + tool calls) |
//! | [`ModelApi::chat_json`] | [`ChatRequest`] + [`StaticOutputSchema`] | [`ChatJsonResponse`] (parsed `T` + tool calls) |
//! | [`ModelApi::infer_media`] | [`MediaRequest`] | `String` (model text about the image) |
//! | [`ModelApi::chat_stream`] | [`ChatRequest`] | [`ChatStream`] of [`ChatStreamEvent`] values |
//!
//! Platform supplies an [`http_client::ClientFactory`]. This crate owns the
//! persistent reqwless connection, reusable buffers, streaming, and retry
//! behavior required by [`ModelApi`].
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
//! [`RetryPolicy`] for the knobs and [`Error::is_retryable`] for the
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
pub use barracuda_agent_message::ChatMessage;
pub use barracuda_runtime_utils::stream;
pub use chat_stream::ChatStream;
pub use client::{ModelApi, ModelApiFactory};
pub use errors::{Error, InitError};
#[cfg(feature = "cache_profile")]
pub use types::ProviderUsage;
pub use types::{
    ChatJsonResponse, ChatRequest, ChatStreamEvent, LlmResponse, MediaAsset, MediaRequest,
    ModelApiConfig, RetryPolicy, StaticOutputSchema, ToolCall,
};

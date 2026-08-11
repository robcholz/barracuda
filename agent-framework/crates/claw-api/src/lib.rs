#![no_std]

//! `claw-api` — LLM client: OpenAI-/Anthropic-compatible chat, structured JSON
//! output, and image inference over an injected HTTP transport.
//!
//! Extracted from `claw_core::llm` into a standalone crate so the LLM client
//! surface can be reused independently of the agent core (e.g. by
//! `claw_memory`'s async extractor and `cap_llm_inspect`).
//!
//! # Overview
//!
//! [`ClawApi`] owns one long-lived reqwless client, including its persistent
//! connection and reusable buffers. Install a complete [`ClawApiConfig`], then issue
//! requests:
//!
//! | Method | Request | Returns |
//! |---|---|---|
//! | [`ClawApi::chat`] | [`ChatRequest`] | [`LlmResponse`] (text + tool calls) |
//! | [`ClawApi::chat_json`] | [`ChatJsonRequest`] | [`ChatJsonResponse`] (parsed `T` + tool calls) |
//! | [`ClawApi::infer_media`] | [`MediaRequest`] | `String` (model text about the image) |
//! | [`ClawApi::chat_stream`] | [`ChatRequest`] | [`ChatStream`] of [`ChatStreamEvent`] values |
//!
//! Networking uses reqwless directly. The application supplies one concrete
//! `embedded_nal_async::TcpConnect + Dns` stack directly to [`ClawApi`]; Embassy
//! and host applications share all HTTP behavior and differ only at the TCP/DNS
//! HAL boundary.
//!
//! # Cancellation
//!
//! Calls take [`claw_utils::Cancel`]. For streaming, that token covers send,
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
//! [`RetryPolicy`] for the knobs and [`ClawApiError::is_retryable`] for the
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
pub use chat_stream::ChatStream;
pub use claw_utils::stream;
pub use client::{ClawApi, ClawApiFactory};
pub use embedded_io::ErrorKind as NetworkErrorKind;
pub use errors::{ChatError, ChatJsonError, ClawApiError, InferMediaError, InitError};
#[cfg(feature = "mbedtls-host")]
pub use mbedtls_rs::Tls;
#[cfg(feature = "mbedtls")]
pub use reqwless::client::TlsConfig;
#[cfg(feature = "embedded-tls")]
pub use reqwless::client::{TlsConfig, TlsVerify};
#[cfg(feature = "mbedtls")]
pub use reqwless::{Certificate, Credentials, TlsReference, TlsVersion, X509};
pub use transport::{Error as HttpError, StatusCode};
#[cfg(feature = "cache_profile")]
pub use types::ProviderUsage;
pub use types::{
    ChatJsonRequest, ChatJsonResponse, ChatRequest, ChatStreamEvent, ClawApiConfig, LlmResponse,
    MediaAsset, MediaRequest, RetryPolicy, StaticOutputSchema, ToolCall,
};

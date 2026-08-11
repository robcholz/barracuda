//! `ClawApi` — the LLM client, port of `claw_llm_runtime.c`.
//!
//! Owns one concrete reqwless client and an optional resolved backend. Construct
//! it with [`ClawApi::new`], then install a complete config with
//! [`ClawApi::set_config`] before issuing requests.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};

use futures_lite::StreamExt as _;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::Instrument as _;

use claw_utils::yield_stream::yield_stream;
use claw_utils::Cancel;
use embedded_nal_async::{Dns, TcpConnect};

use super::backends::Backend;
use super::chat_stream::{ChatStream, Driver, DriverItem};
use super::errors::{ChatError, ChatJsonError, ClawApiError, InferMediaError, InitError};
use super::retry::{sleep_abortable_async, with_timeout, DeadlineError};
use super::transport::{Error as HttpError, HttpTransport};
use super::types::{
    ChatJsonRequest, ChatJsonResponse, ChatRequest, ClawApiConfig, LlmResponse, MediaRequest,
};

/// LLM client backed directly by one exclusively owned reqwless client.
pub struct ClawApi<'net, S: TcpConnect + Dns + 'net> {
    backend: Option<Backend>,
    http: HttpTransport<'net, S>,
}

/// Application-supplied constructor for independent, fully configured client
/// resources. It owns no HTTP behavior; each call returns one concrete
/// [`ClawApi`] with its own reqwless state and buffers.
pub struct ClawApiFactory<S: TcpConnect + Dns + 'static> {
    make: Rc<dyn Fn() -> ClawApi<'static, S>>,
}

impl<S: TcpConnect + Dns + 'static> Clone for ClawApiFactory<S> {
    fn clone(&self) -> Self {
        Self {
            make: Rc::clone(&self.make),
        }
    }
}

impl<S: TcpConnect + Dns + 'static> ClawApiFactory<S> {
    #[must_use]
    pub fn new(make: impl Fn() -> ClawApi<'static, S> + 'static) -> Self {
        Self {
            make: Rc::new(make),
        }
    }

    #[must_use]
    pub fn create(&self) -> ClawApi<'static, S> {
        (self.make)()
    }
}

fn resolve_config(config: ClawApiConfig) -> Result<Backend, InitError> {
    // Backends trust that required fields are present once `set_config` returns.
    config.validate()?;
    config.backend.make(&config)
}

fn parse_chat_json_response<T: DeserializeOwned>(
    response: LlmResponse,
) -> Result<ChatJsonResponse<T>, ChatJsonError> {
    let output = match response.text {
        Some(ref text) if !text.trim().is_empty() => Some(
            serde_json::from_str(text)
                .map_err(|err| ChatJsonError::InvalidOutput(err.to_string()))?,
        ),
        _ => None,
    };

    if output.is_none() && response.tool_calls.is_empty() {
        return Err(ChatJsonError::EmptyText);
    }

    Ok(ChatJsonResponse {
        output,
        tool_calls: response.tool_calls,
        reasoning_content: response.reasoning_content,
        raw_message_json: response.raw_message_json,
    })
}

/// Stable, shape-only trace classification; never expose an error's payload.
fn chat_error_kind(error: &ChatError) -> &'static str {
    match error {
        ChatError::Api(error) => error.into(),
        other => other.into(),
    }
}

fn deadline_error(error: DeadlineError) -> ClawApiError {
    match error {
        DeadlineError::Cancelled => ClawApiError::Transport(HttpError::Cancelled),
        DeadlineError::Elapsed => ClawApiError::Timeout,
    }
}

fn retrying_chat_stream<'h, 'r, S>(
    backend: &'h Backend,
    http: &'h mut HttpTransport<'_, S>,
    request: &'r ChatRequest<'r>,
    cancel: Cancel<'h>,
) -> Driver<'h>
where
    S: TcpConnect + Dns,
    'r: 'h,
{
    let policy = request.retry;
    let max_attempts = u64::from(policy.max_retries).saturating_add(1);
    yield_stream(|yielder| async move {
        let mut retry_attempt = 0_u32;
        let mut emitted = false;
        let mut opened = false;

        'request: loop {
            let attempt = u64::from(retry_attempt).saturating_add(1);
            let attempt_span = tracing::info_span!("api.attempt", attempt, max_attempts);

            let (error, phase) = 'attempt: {
                let opened_stream = with_timeout(
                    backend.chat_stream_async(http, request, cancel),
                    backend.timeout_ms(),
                    cancel,
                )
                .instrument(attempt_span.clone())
                .await
                .map_err(deadline_error)
                .map_err(ChatError::from)
                .and_then(core::convert::identity);
                let mut stream = match opened_stream {
                    Ok(stream) => {
                        attempt_span.in_scope(|| tracing::info!(name: "opened", ""));
                        stream
                    }
                    Err(error) => break 'attempt (error, "open"),
                };

                if !opened {
                    opened = true;
                    yielder.yield_one(DriverItem::Opened).await;
                }

                loop {
                    let next = with_timeout(stream.next(), backend.timeout_ms(), cancel)
                        .instrument(attempt_span.clone())
                        .await;
                    match next {
                        Err(error) => {
                            break 'attempt (ChatError::from(deadline_error(error)), "body")
                        }
                        Ok(Some(Ok(event))) => {
                            emitted = true;
                            yielder.yield_one(DriverItem::Event(Ok(event))).await;
                        }
                        Ok(Some(Err(error))) => break 'attempt (error, "body"),
                        Ok(None) => {
                            attempt_span.in_scope(|| tracing::info!(name: "completed", ""));
                            return;
                        }
                    }
                }
            };

            let retryable = error.is_retryable();
            let replay_safe = !emitted;
            let final_attempt = !retryable || !replay_safe || retry_attempt >= policy.max_retries;
            let kind = chat_error_kind(&error);
            attempt_span.in_scope(|| {
                if final_attempt {
                    tracing::error!(
                        name: "failed",
                        kind,
                        phase,
                        retryable,
                        replay_safe,
                        final = true
                    );
                } else {
                    tracing::warn!(
                        name: "failed",
                        kind,
                        phase,
                        retryable,
                        replay_safe,
                        final = false
                    );
                }
            });

            if final_attempt {
                yielder.yield_one(DriverItem::Event(Err(error))).await;
                return;
            }

            let failed_attempt = attempt;
            retry_attempt = retry_attempt.saturating_add(1);
            let next_attempt = u64::from(retry_attempt).saturating_add(1);
            let backoff_ms = policy.backoff_ms(retry_attempt);
            let completed = async {
                let completed = sleep_abortable_async(backoff_ms, cancel).await;
                if completed {
                    tracing::info!(name: "completed", "");
                } else {
                    tracing::warn!(name: "cancelled", "");
                }
                completed
            }
            .instrument(tracing::info_span!(
                "api.retry",
                failed_attempt,
                next_attempt,
                backoff_ms,
                error_kind = kind,
                phase
            ))
            .await;
            if !completed {
                yielder
                    .yield_one(DriverItem::Event(Err(ChatError::Api(
                        ClawApiError::Transport(HttpError::Cancelled),
                    ))))
                    .await;
                return;
            }

            continue 'request;
        }
    })
}

impl<'net, S: TcpConnect + Dns + 'net> ClawApi<'net, S> {
    /// Construct an unconfigured client over the supplied reqwless transport.
    #[must_use]
    pub fn new(network: &'net S, header_buffer_size: usize, read_buffer_size: usize) -> Self {
        Self {
            backend: None,
            http: HttpTransport::new(network, header_buffer_size, read_buffer_size),
        }
    }

    /// Construct an unconfigured HTTPS client over the supplied network stack.
    #[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
    #[must_use]
    pub fn new_with_tls(
        network: &'net S,
        tls: reqwless::client::TlsConfig<'net>,
        header_buffer_size: usize,
        read_buffer_size: usize,
    ) -> Self {
        Self {
            backend: None,
            http: HttpTransport::new_with_tls(network, tls, header_buffer_size, read_buffer_size),
        }
    }

    /// Rebind this client to a new [`ClawApiConfig`] at runtime, keeping the
    /// existing reqwless client and reusable buffers. Only the backend —
    /// provider, key, model, base URL — is rebuilt.
    ///
    /// Used to apply a per-turn config selected from a `ClawApiManager` without
    /// reconstructing the whole client. Returns [`InitError`] if the new config
    /// is incomplete or invalid.
    pub fn set_config(&mut self, config: ClawApiConfig) -> Result<(), InitError> {
        self.backend = Some(resolve_config(config)?);
        Ok(())
    }

    /// Async chat completion over the owned reqwless client.
    pub async fn chat(
        &mut self,
        request: &ChatRequest<'_>,
        cancel: Cancel<'_>,
    ) -> Result<LlmResponse, ChatError> {
        let backend = self
            .backend
            .as_ref()
            .ok_or(ClawApiError::NotConfigured)
            .map_err(ChatError::from)?;
        let policy = request.retry;
        let max_attempts = u64::from(policy.max_retries).saturating_add(1);
        let mut retry_attempt = 0u32;
        loop {
            let attempt = u64::from(retry_attempt).saturating_add(1);
            let result = async {
                let result = with_timeout(
                    backend.chat_async(&mut self.http, request, cancel),
                    backend.timeout_ms(),
                    cancel,
                )
                .await
                .map_err(deadline_error)
                .map_err(ChatError::from)
                .and_then(core::convert::identity);
                match &result {
                    Ok(_) => tracing::info!(name: "completed", ""),
                    Err(error) => {
                        let kind = chat_error_kind(error);
                        let retryable = error.is_retryable();
                        let final_attempt = !retryable || retry_attempt >= policy.max_retries;
                        if final_attempt {
                            tracing::error!(
                                name: "failed",
                                kind,
                                retryable,
                                final = true
                            );
                        } else {
                            tracing::warn!(
                                name: "failed",
                                kind,
                                retryable,
                                final = false
                            );
                        }
                    }
                }
                result
            }
            .instrument(tracing::info_span!("api.attempt", attempt, max_attempts))
            .await;

            match result {
                Ok(response) => return Ok(response),
                Err(error) => {
                    if !error.is_retryable() || retry_attempt >= policy.max_retries {
                        return Err(error);
                    }
                    let error_kind = chat_error_kind(&error);
                    let failed_attempt = attempt;
                    retry_attempt = retry_attempt.saturating_add(1);
                    let next_attempt = u64::from(retry_attempt).saturating_add(1);
                    let backoff_ms = policy.backoff_ms(retry_attempt);
                    let completed = async {
                        let completed = sleep_abortable_async(backoff_ms, cancel).await;
                        if completed {
                            tracing::info!(name: "completed", "");
                        } else {
                            tracing::warn!(name: "cancelled", "");
                        }
                        completed
                    }
                    .instrument(tracing::info_span!(
                        "api.retry",
                        failed_attempt,
                        next_attempt,
                        backoff_ms,
                        error_kind
                    ))
                    .await;
                    if !completed {
                        return Err(ChatError::Api(ClawApiError::Transport(
                            HttpError::Cancelled,
                        )));
                    }
                }
            }
        }
    }

    /// Streaming chat completion over the owned reqwless client.
    ///
    /// Yields [`ChatStreamEvent`](crate::ChatStreamEvent) values as reasoning,
    /// output, and tool-call logical streams of
    /// [`StreamPart`](claw_utils::stream::StreamPart). Unlike
    /// [`chat`](Self::chat), it never assembles an [`LlmResponse`]. Transient
    /// open or body failures are retried according to `request.retry` only until
    /// the first semantic event is yielded. After that boundary replay could
    /// duplicate caller-visible output, so every failure is terminal. `cancel`
    /// remains active for opening, retry backoff, and the full body stream. The
    /// configured timeout applies to opening and to every provider-event read.
    pub async fn chat_stream<'h, 'r>(
        &'h mut self,
        request: &'r ChatRequest<'r>,
        cancel: Cancel<'h>,
    ) -> Result<ChatStream<'h>, ChatError>
    where
        'r: 'h,
    {
        let Self { backend, http } = self;
        let backend = backend.as_ref().ok_or(ClawApiError::NotConfigured)?;
        ChatStream::open(retrying_chat_stream(backend, http, request, cancel)).await
    }

    /// Async structured JSON chat over the owned reqwless client.
    pub async fn chat_json<Output: DeserializeOwned>(
        &mut self,
        request: &ChatJsonRequest<'_>,
        cancel: Cancel<'_>,
    ) -> Result<ChatJsonResponse<Output>, ChatJsonError> {
        let backend = self
            .backend
            .as_ref()
            .ok_or(ClawApiError::NotConfigured)
            .map_err(ChatError::from)?;
        let spec = request
            .output_schema
            .ok_or(ChatJsonError::MissingOutputSchema)?;
        let schema: Value = serde_json::from_str(spec.json)
            .map_err(|err| ChatJsonError::InvalidOutput(format!("invalid schema json: {err}")))?;

        let policy = request.retry;
        let mut attempt = 0u32;
        loop {
            let timed = with_timeout(
                backend.chat_json_async(&mut self.http, request, spec.name, &schema, cancel),
                backend.timeout_ms(),
                cancel,
            )
            .await
            .map_err(deadline_error)
            .map_err(ChatError::from)
            .and_then(core::convert::identity);
            let result = match timed {
                Ok(response) => parse_chat_json_response(response),
                Err(error) => Err(ChatJsonError::from(error)),
            };

            match result {
                Ok(response) => return Ok(response),
                Err(error) => {
                    if !error.is_retryable() || attempt >= policy.max_retries {
                        return Err(error);
                    }
                    attempt = attempt.saturating_add(1);
                    if !sleep_abortable_async(policy.backoff_ms(attempt), cancel).await {
                        return Err(ChatJsonError::Chat(ChatError::Api(
                            ClawApiError::Transport(HttpError::Cancelled),
                        )));
                    }
                }
            }
        }
    }

    /// Async one-shot image inference over the owned reqwless client.
    pub async fn infer_media(
        &mut self,
        request: &MediaRequest<'_>,
        cancel: Cancel<'_>,
    ) -> Result<String, InferMediaError> {
        let backend = self.backend.as_ref().ok_or(ClawApiError::NotConfigured)?;
        let policy = request.retry;
        let mut attempt = 0u32;
        loop {
            let result = with_timeout(
                backend.infer_media_async(&mut self.http, request, cancel),
                backend.timeout_ms(),
                cancel,
            )
            .await
            .map_err(deadline_error)
            .map_err(InferMediaError::from)
            .and_then(core::convert::identity);
            match result {
                Ok(response) => return Ok(response),
                Err(error) => {
                    if !error.is_retryable() || attempt >= policy.max_retries {
                        return Err(error);
                    }
                    attempt = attempt.saturating_add(1);
                    if !sleep_abortable_async(policy.backoff_ms(attempt), cancel).await {
                        return Err(InferMediaError::Api(ClawApiError::Transport(
                            HttpError::Cancelled,
                        )));
                    }
                }
            }
        }
    }
}

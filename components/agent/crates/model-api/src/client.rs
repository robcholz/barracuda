//! `ModelApi` — the LLM client.
//!
//! Owns one concrete reqwless client and an optional resolved backend. Construct
//! it with [`ModelApi::new`], then install a complete config with
//! [`ModelApi::set_config`] before issuing requests.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};

use futures_lite::StreamExt as _;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::Instrument as _;

use barracuda_runtime_utils::yield_stream::yield_stream;
use barracuda_runtime_utils::Cancel;
use embedded_nal_async::{Dns, TcpConnect};

use super::backends::Backend;
use super::chat_stream::{ChatStream, Driver, DriverItem};
use super::errors::{ChatError, ChatJsonError, InferMediaError, InitError, ModelApiError};
use super::retry::{deadline_error, retry_call, sleep_or_cancel, timed, with_timeout, RetryState};
use super::transport::{Error as HttpError, HttpTransport};
use super::types::{
    ChatJsonRequest, ChatJsonResponse, ChatRequest, LlmResponse, MediaRequest, ModelApiConfig,
};

/// LLM client backed directly by one exclusively owned reqwless client.
pub struct ModelApi<'net, S: TcpConnect + Dns + 'net> {
    backend: Option<Backend>,
    http: HttpTransport<'net, S>,
}

/// Application-supplied constructor for independent, fully configured client
/// resources. It owns no HTTP behavior; each call returns one concrete
/// [`ModelApi`] with its own reqwless state and buffers.
pub struct ModelApiFactory<S: TcpConnect + Dns + 'static> {
    make: Rc<dyn Fn() -> ModelApi<'static, S>>,
}

impl<S: TcpConnect + Dns + 'static> Clone for ModelApiFactory<S> {
    // A derived impl would unnecessarily require `S: Clone`; cloning the
    // factory only increments the `Rc` count.
    fn clone(&self) -> Self {
        Self {
            make: Rc::clone(&self.make),
        }
    }
}

impl<S: TcpConnect + Dns + 'static> ModelApiFactory<S> {
    #[must_use]
    pub fn new(make: impl Fn() -> ModelApi<'static, S> + 'static) -> Self {
        Self {
            make: Rc::new(make),
        }
    }

    #[must_use]
    pub fn create(&self) -> ModelApi<'static, S> {
        (self.make)()
    }
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
    yield_stream(|yielder| async move {
        let mut retry = RetryState::new(request.retry);
        let mut emitted = false;
        let mut opened = false;

        'request: loop {
            let attempt = retry.attempt();
            let attempt_span =
                tracing::info_span!("api.attempt", attempt, max_attempts = retry.max_attempts());

            let (error, phase) = 'attempt: {
                let opened_stream = timed(
                    backend.chat_stream(http, request),
                    backend.timeout_ms(),
                    cancel,
                )
                .instrument(attempt_span.clone())
                .await;
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
            let final_attempt = !replay_safe || !retry.can_retry(retryable);
            let kind = error.kind();
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

            let delay = retry.advance();
            let completed = async {
                let completed = sleep_or_cancel(delay.backoff_ms, cancel).await;
                if completed {
                    tracing::info!(name: "completed", "");
                } else {
                    tracing::warn!(name: "cancelled", "");
                }
                completed
            }
            .instrument(tracing::info_span!(
                "api.retry",
                failed_attempt = delay.failed_attempt,
                next_attempt = delay.next_attempt,
                backoff_ms = delay.backoff_ms,
                error_kind = kind,
                phase
            ))
            .await;
            if !completed {
                yielder
                    .yield_one(DriverItem::Event(Err(ChatError::Api(
                        ModelApiError::Transport(HttpError::Cancelled),
                    ))))
                    .await;
                return;
            }

            continue 'request;
        }
    })
}

impl<'net, S: TcpConnect + Dns + 'net> ModelApi<'net, S> {
    fn with_transport(http: HttpTransport<'net, S>) -> Self {
        Self {
            backend: None,
            http,
        }
    }

    /// Construct an unconfigured client over the supplied reqwless transport.
    #[must_use]
    pub fn new(network: &'net S, header_buffer_size: usize, read_buffer_size: usize) -> Self {
        Self::with_transport(HttpTransport::new(
            network,
            header_buffer_size,
            read_buffer_size,
        ))
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
        Self::with_transport(HttpTransport::new_with_tls(
            network,
            tls,
            header_buffer_size,
            read_buffer_size,
        ))
    }

    /// Rebind this client to a new [`ModelApiConfig`] at runtime, keeping the
    /// existing reqwless client and reusable buffers. Only the backend —
    /// provider, key, model, base URL — is rebuilt.
    ///
    /// Used to apply a per-turn config selected from a `ModelApiManager` without
    /// reconstructing the whole client. Returns [`InitError`] if the new config
    /// is incomplete or invalid.
    pub fn set_config(&mut self, config: ModelApiConfig) -> Result<(), InitError> {
        config.validate()?;
        self.backend = Some(config.backend.make(config));
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
            .ok_or(ModelApiError::NotConfigured)
            .map_err(ChatError::from)?;
        let mut retry = RetryState::new(request.retry);
        loop {
            let attempt = retry.attempt();
            let result = async {
                let result = timed(
                    backend.chat(&mut self.http, request),
                    backend.timeout_ms(),
                    cancel,
                )
                .await;
                match &result {
                    Ok(_) => tracing::info!(name: "completed", ""),
                    Err(error) => {
                        let kind = error.kind();
                        let retryable = error.is_retryable();
                        let final_attempt = !retry.can_retry(retryable);
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
            .instrument(tracing::info_span!(
                "api.attempt",
                attempt,
                max_attempts = retry.max_attempts()
            ))
            .await;

            match result {
                Ok(response) => return Ok(response),
                Err(error) => {
                    if !retry.can_retry(error.is_retryable()) {
                        return Err(error);
                    }
                    let error_kind = error.kind();
                    let delay = retry.advance();
                    let completed = async {
                        let completed = sleep_or_cancel(delay.backoff_ms, cancel).await;
                        if completed {
                            tracing::info!(name: "completed", "");
                        } else {
                            tracing::warn!(name: "cancelled", "");
                        }
                        completed
                    }
                    .instrument(tracing::info_span!(
                        "api.retry",
                        failed_attempt = delay.failed_attempt,
                        next_attempt = delay.next_attempt,
                        backoff_ms = delay.backoff_ms,
                        error_kind
                    ))
                    .await;
                    if !completed {
                        return Err(ChatError::Api(ModelApiError::Transport(
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
    /// [`StreamPart`](barracuda_runtime_utils::stream::StreamPart). Unlike
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
        let backend = backend.as_ref().ok_or(ModelApiError::NotConfigured)?;
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
            .ok_or(ModelApiError::NotConfigured)
            .map_err(ChatError::from)?;
        let spec = request
            .output_schema
            .ok_or(ChatJsonError::MissingOutputSchema)?;
        let schema: Value = serde_json::from_str(spec.json)
            .map_err(|err| ChatJsonError::InvalidOutput(format!("invalid schema json: {err}")))?;

        retry_call(request.chat.retry, cancel, async || {
            let response = timed(
                backend.chat_json(&mut self.http, request, spec.name, &schema),
                backend.timeout_ms(),
                cancel,
            )
            .await?;
            parse_chat_json_response(response)
        })
        .await
    }

    /// Async one-shot image inference over the owned reqwless client.
    pub async fn infer_media(
        &mut self,
        request: &MediaRequest<'_>,
        cancel: Cancel<'_>,
    ) -> Result<String, InferMediaError> {
        let backend = self.backend.as_ref().ok_or(ModelApiError::NotConfigured)?;
        retry_call(request.retry, cancel, async || {
            timed(
                backend.infer_media(&mut self.http, request),
                backend.timeout_ms(),
                cancel,
            )
            .await
        })
        .await
    }
}

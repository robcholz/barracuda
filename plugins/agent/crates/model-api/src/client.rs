//! `ModelApi` — the LLM client.
//!
//! Owns one persistent HTTP transport and an optional resolved backend.
//! Construct it with [`ModelApi::new`], then install a complete config with
//! [`ModelApi::set_config`] before issuing requests.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};

use embedded_nal_async::{Dns, TcpConnect};
use futures_lite::StreamExt as _;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::Instrument as _;

use barracuda_runtime_utils::yield_stream::yield_stream;
use barracuda_runtime_utils::Cancel;

use super::backends::Backend;
use super::chat_stream::{ChatStream, Driver, DriverItem};
use super::errors::{Error, InitError};
use super::retry::{deadline_error, retry_call, sleep_or_cancel, timed, with_timeout, RetryState};
use super::transport::Transport;
use super::types::{
    ChatJsonResponse, ChatRequest, LlmResponse, MediaRequest, ModelApiConfig, StaticOutputSchema,
};

/// LLM client backed by one Agent-owned persistent HTTP transport.
pub struct ModelApi<'net, Tcp = http_client::Tcp, Resolver = http_client::Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    backend: Option<Backend>,
    http: Transport<'net, Tcp, Resolver>,
}

/// Application-supplied constructor for independent, fully configured client
/// resources. It owns no HTTP behavior; each call returns one [`ModelApi`]
/// whose transport lifecycle is hidden behind the factory.
pub struct ModelApiFactory<Tcp = http_client::Tcp, Resolver = http_client::Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    make: Rc<dyn Fn() -> ModelApi<'static, Tcp, Resolver>>,
}

impl<Tcp, Resolver> Clone for ModelApiFactory<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    // A derived impl would unnecessarily require the transports to be Clone;
    // cloning the factory only increments the Rc count.
    fn clone(&self) -> Self {
        Self {
            make: Rc::clone(&self.make),
        }
    }
}

impl<Tcp, Resolver> ModelApiFactory<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    #[must_use]
    pub fn new(make: impl Fn() -> ModelApi<'static, Tcp, Resolver> + 'static) -> Self {
        Self {
            make: Rc::new(make),
        }
    }

    #[must_use]
    pub fn create(&self) -> ModelApi<'static, Tcp, Resolver> {
        (self.make)()
    }
}

fn parse_chat_json_response<T: DeserializeOwned>(
    response: LlmResponse,
) -> Result<ChatJsonResponse<T>, Error> {
    let output = match response.text {
        Some(ref text) if !text.trim().is_empty() => Some(
            serde_json::from_str(text)
                .map_err(|err| Error::InvalidStructuredOutput(err.to_string()))?,
        ),
        _ => None,
    };

    if output.is_none() && response.tool_calls.is_empty() {
        return Err(Error::EmptyStructuredOutput);
    }

    Ok(ChatJsonResponse {
        output,
        tool_calls: response.tool_calls,
        reasoning_content: response.reasoning_content,
        raw_message_json: response.raw_message_json,
    })
}

fn retrying_chat_stream<'h, 'r, Tcp, Resolver>(
    backend: &'h Backend,
    http: &'h mut Transport<'_, Tcp, Resolver>,
    request: &'r ChatRequest<'r>,
    cancel: Cancel<'h>,
) -> Driver<'h>
where
    'r: 'h,
    Tcp: TcpConnect,
    Resolver: Dns,
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
                            break 'attempt (deadline_error(error), "body");
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
                    .yield_one(DriverItem::Event(Err(Error::Cancelled)))
                    .await;
                return;
            }

            continue 'request;
        }
    })
}

impl<'net, Tcp, Resolver> ModelApi<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    /// Constructs an unconfigured model client from shared HTTP resources.
    #[must_use]
    pub fn new(http_clients: http_client::ClientFactory<'net, Tcp, Resolver>) -> Self {
        Self {
            backend: None,
            http: Transport::new(http_clients),
        }
    }

    /// Rebind this client to a new [`ModelApiConfig`] at runtime, keeping the
    /// existing Agent HTTP client and reusable buffers. Only the backend —
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

    /// Async chat completion over the owned Agent HTTP client.
    pub async fn chat(
        &mut self,
        request: &ChatRequest<'_>,
        cancel: Cancel<'_>,
    ) -> Result<LlmResponse, Error> {
        let backend = self.backend.as_ref().ok_or(Error::NotConfigured)?;
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
                        return Err(Error::Cancelled);
                    }
                }
            }
        }
    }

    /// Streaming chat completion over the owned Agent HTTP client.
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
    ) -> Result<ChatStream<'h>, Error>
    where
        'r: 'h,
    {
        let Self { backend, http } = self;
        let backend = backend.as_ref().ok_or(Error::NotConfigured)?;
        ChatStream::open(retrying_chat_stream(backend, http, request, cancel)).await
    }

    /// Async structured JSON chat over the owned Agent HTTP client.
    pub async fn chat_json<Output: DeserializeOwned>(
        &mut self,
        request: &ChatRequest<'_>,
        output_schema: StaticOutputSchema<'_>,
        cancel: Cancel<'_>,
    ) -> Result<ChatJsonResponse<Output>, Error> {
        let backend = self.backend.as_ref().ok_or(Error::NotConfigured)?;
        let schema: Value = serde_json::from_str(output_schema.json)
            .map_err(|err| Error::InvalidStructuredOutput(format!("invalid schema json: {err}")))?;

        retry_call(request.retry, cancel, async || {
            let response = timed(
                backend.chat_json(&mut self.http, request, output_schema.name, &schema),
                backend.timeout_ms(),
                cancel,
            )
            .await?;
            parse_chat_json_response(response)
        })
        .await
    }

    /// Async one-shot image inference over the owned Agent HTTP client.
    pub async fn infer_media(
        &mut self,
        request: &MediaRequest<'_>,
        cancel: Cancel<'_>,
    ) -> Result<String, Error> {
        let backend = self.backend.as_ref().ok_or(Error::NotConfigured)?;
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

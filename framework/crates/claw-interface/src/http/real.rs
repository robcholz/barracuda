use core::future::Future as _;
use core::pin::Pin;
use core::task::Poll;
use std::time::Duration;

use futures_lite::StreamExt as _;

use super::{
    cancel_on_poll, Cancel, ClawHttp, HttpError, HttpJsonRequest, HttpRequestFailure, HttpResponse,
    HttpResponseFuture, HttpStatusCode, Stream, StreamingHttp,
};

pub(super) mod blocking {
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::super::{
        blocking::ClawHttp, HttpError, HttpJsonRequest, HttpRequestFailure, HttpResponse,
        HttpStatusCode,
    };

    /// Host [`ClawHttp`] backed by a blocking `reqwest::blocking::Client`.
    ///
    /// This is the synchronous compatibility backend for host CLIs and live
    /// tests that intentionally run blocking HTTP. Async code should prefer the
    /// top-level [`crate::http::RealHttp`].
    #[derive(Debug, Clone, Default)]
    pub struct RealHttp {
        user_agent: Option<String>,
    }

    impl RealHttp {
        /// A client with no extra `User-Agent`.
        pub fn new() -> Self {
            Self::default()
        }

        /// A client that sends the given `User-Agent` on every request.
        pub fn with_user_agent(user_agent: impl Into<String>) -> Self {
            Self {
                user_agent: Some(user_agent.into()),
            }
        }
    }

    impl ClawHttp for RealHttp {
        fn post_json(
            &mut self,
            request: &HttpJsonRequest,
            abort: &AtomicBool,
        ) -> Result<HttpResponse, HttpError> {
            if abort.load(Ordering::Acquire) {
                return Err(HttpError::Aborted);
            }

            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_millis(u64::from(request.timeout_ms)))
                .build()
                .map_err(|_| HttpError::ClientInitFailed)?;

            let mut builder = client
                .post(request.url)
                .header("Content-Type", "application/json")
                .body(request.body.to_string());

            if let Some(user_agent) = &self.user_agent {
                builder = builder.header("User-Agent", user_agent);
            }

            if let Some((name, value)) = request.auth.header() {
                builder = builder.header(name, value);
            }
            for header in request.headers {
                builder = builder.header(header.name, header.value);
            }

            let response = builder.send().map_err(|error| {
                HttpError::RequestFailed(HttpRequestFailure::transport(error.to_string()))
            })?;
            let status_code = HttpStatusCode::new(response.status().as_u16());
            let body = response.text().map_err(|error| {
                HttpError::RequestFailed(HttpRequestFailure::transport(error.to_string()))
            })?;

            if !status_code.is_success() {
                return Err(HttpError::UnexpectedStatus {
                    status: status_code,
                    message: format!("HTTP {status_code}: {body}"),
                });
            }
            Ok(HttpResponse { status_code, body })
        }
    }
}

/// Host [`ClawHttp`] backed by an **async** `reqwest::Client`.
///
/// The native non-blocking counterpart of [`blocking::RealHttp`]: it
/// issues a genuinely concurrent request instead of blocking the polling
/// task, so multiple in-flight calls progress together. It honours
/// `request.auth` (bearer / API key / none), forwards extra
/// headers, and treats any 2xx as success. Cancellation is handled by
/// [`ClawHttp::post_json`] by dropping the in-flight reqwest future.
///
/// Driver requirement: `reqwest`'s futures poll against **tokio**'s IO
/// reactor, so this backend must be driven by a tokio runtime
/// (`Runtime::block_on` or a tokio test runtime) — *not* by the cooperative
/// `embedded-executor` used for the device-model `YieldingHttpAdapter`
/// futures (those have no reactor). This keeps it strictly a host backend
/// (CLIs, integration tests); on-device async HTTP uses the `esp_http_client`
/// driver in `claw_sys` instead.
///
/// The `reqwest::Client` pools connections, so construct one and reuse it.
#[derive(Debug, Clone, Default)]
pub struct RealHttp {
    client: reqwest::Client,
    user_agent: Option<String>,
}

impl RealHttp {
    /// A client with no extra `User-Agent`.
    pub fn new() -> Self {
        Self::default()
    }

    /// A client that sends the given `User-Agent` on every request.
    pub fn with_user_agent(user_agent: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            user_agent: Some(user_agent.into()),
        }
    }
}

impl ClawHttp for RealHttp {
    fn post_json<'a>(
        &'a mut self,
        request: &'a HttpJsonRequest<'a>,
        cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        let transfer: HttpResponseFuture<'a> = Box::pin(async move {
            let mut builder = self
                .client
                .post(request.url)
                .header("Content-Type", "application/json")
                .timeout(Duration::from_millis(u64::from(request.timeout_ms)))
                .body(request.body.to_string());

            if let Some(user_agent) = &self.user_agent {
                builder = builder.header("User-Agent", user_agent);
            }

            if let Some((name, value)) = request.auth.header() {
                builder = builder.header(name, value);
            }
            for header in request.headers {
                builder = builder.header(header.name, header.value);
            }

            let response = builder.send().await.map_err(|error| {
                HttpError::RequestFailed(HttpRequestFailure::transport(error.to_string()))
            })?;
            let status_code = HttpStatusCode::new(response.status().as_u16());
            let body = response.text().await.map_err(|error| {
                HttpError::RequestFailed(HttpRequestFailure::transport(error.to_string()))
            })?;

            if !status_code.is_success() {
                return Err(HttpError::UnexpectedStatus {
                    status: status_code,
                    message: format!("HTTP {status_code}: {body}"),
                });
            }
            Ok(HttpResponse { status_code, body })
        });
        cancel_on_poll(transfer, cancel)
    }
}

impl StreamingHttp for RealHttp {
    type ByteStream<'a>
        = Pin<Box<dyn Stream<Item = Result<Vec<u8>, HttpError>> + Send + 'a>>
    where
        Self: 'a;

    async fn post_json_streaming<'a, 'r>(
        &'a mut self,
        request: &'r HttpJsonRequest<'r>,
        cancel: Cancel<'a>,
    ) -> Result<(HttpStatusCode, Self::ByteStream<'a>), HttpError> {
        let mut builder = self
            .client
            .post(request.url)
            .header("Content-Type", "application/json")
            .timeout(Duration::from_millis(u64::from(request.timeout_ms)))
            .body(request.body.to_string());
        if let Some(user_agent) = &self.user_agent {
            builder = builder.header("User-Agent", user_agent);
        }
        if let Some((name, value)) = request.auth.header() {
            builder = builder.header(name, value);
        }
        for header in request.headers {
            builder = builder.header(header.name, header.value);
        }

        let mut send = core::pin::pin!(builder.send());
        let response = core::future::poll_fn(|context| {
            if cancel.is_cancelled() {
                return Poll::Ready(Err(HttpError::Aborted));
            }
            match send.as_mut().poll(context) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(response)) => Poll::Ready(Ok(response)),
                Poll::Ready(Err(error)) => Poll::Ready(Err(HttpError::RequestFailed(
                    HttpRequestFailure::transport(error.to_string()),
                ))),
            }
        })
        .await?;
        let status = HttpStatusCode::new(response.status().as_u16());
        let stream = response.bytes_stream().map(|chunk| {
            chunk.map(|bytes| bytes.to_vec()).map_err(|error| {
                HttpError::RequestFailed(HttpRequestFailure::transport(error.to_string()))
            })
        });
        let mut stream = Box::pin(stream);
        let mut terminated = false;
        let stream = futures_lite::stream::poll_fn(move |context| {
            if terminated {
                return Poll::Ready(None);
            }
            if cancel.is_cancelled() {
                terminated = true;
                return Poll::Ready(Some(Err(HttpError::Aborted)));
            }
            match stream.as_mut().poll_next(context) {
                Poll::Ready(None) => {
                    terminated = true;
                    Poll::Ready(None)
                }
                ready => ready,
            }
        });
        Ok((status, Box::pin(stream) as Self::ByteStream<'a>))
    }
}

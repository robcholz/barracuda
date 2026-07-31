//! Blocking, async, and streaming HTTP dependency-injection traits.
//!
//! Replaces `claw_llm_http_transport.c`. The espidf wiring implements this over
//! `esp_http_client`; host tests provide canned responses.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Poll;

use futures_core::Stream;

#[cfg(feature = "realhttp")]
mod real;
#[cfg(feature = "httpmock")]
mod testing;

#[cfg(feature = "realhttp")]
pub use real::RealHttp;
#[cfg(feature = "httpmock")]
pub use testing::{
    BlockingHttpAdapter, CapturingHttp, ChunkedHttp, FailingHttp, NeverHttp, NoopHttp, ScriptStep,
    ScriptedHttp, SharedScriptHttp, SliceChunks, YieldingHttpAdapter,
};

/// A single extra request header (`name: value`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpHeader<'a> {
    pub name: &'a str,
    pub value: &'a str,
}

/// Authentication to apply to an HTTP request.
///
/// This keeps the key and its wire format in one value, so callers cannot build
/// invalid combinations such as "auth disabled but key present".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HttpAuth<'a> {
    /// Send no authentication header.
    #[default]
    None,
    /// Send `Authorization: Bearer <key>`. Empty keys send no auth header.
    Bearer(&'a str),
    /// Send `X-API-Key: <key>`. Empty keys send no auth header.
    ApiKey(&'a str),
}

impl<'a> HttpAuth<'a> {
    /// Build the wire header for this auth mode, if any.
    pub fn header(self) -> Option<(&'static str, String)> {
        match self {
            Self::None => None,
            Self::Bearer("") | Self::ApiKey("") => None,
            Self::Bearer(key) => Some(("Authorization", format!("Bearer {key}"))),
            Self::ApiKey(key) => Some(("X-API-Key", key.to_string())),
        }
    }
}

/// Parameters for a JSON POST, mirroring `claw_llm_http_json_request_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpJsonRequest<'a> {
    pub url: &'a str,
    pub body: &'a str,
    pub auth: HttpAuth<'a>,
    pub timeout_ms: u32,
    pub headers: &'a [HttpHeader<'a>],
}

/// Parameters for a JSON GET request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpGetRequest<'a> {
    pub url: &'a str,
    pub auth: HttpAuth<'a>,
    pub timeout_ms: u32,
    pub headers: &'a [HttpHeader<'a>],
}

/// HTTP status code.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HttpStatusCode(u16);

impl HttpStatusCode {
    pub const OK: Self = Self(200);
    pub const NO_CONTENT: Self = Self(204);

    pub const fn new(code: u16) -> Self {
        Self(code)
    }

    pub const fn as_u16(self) -> u16 {
        self.0
    }

    pub fn as_i32(self) -> i32 {
        i32::from(self.0)
    }

    pub const fn is_success(self) -> bool {
        self.0 >= 200 && self.0 < 300
    }
}

impl fmt::Display for HttpStatusCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<HttpStatusCode> for u16 {
    fn from(status: HttpStatusCode) -> Self {
        status.as_u16()
    }
}

impl From<HttpStatusCode> for i32 {
    fn from(status: HttpStatusCode) -> Self {
        status.as_i32()
    }
}

/// A successful HTTP response, mirroring `claw_llm_http_response_t`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status_code: HttpStatusCode,
    pub body: String,
}

/// Structured reason for a transport-level request failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpRequestFailure {
    #[error("transport failed: {0}")]
    Transport(String),
    #[error("{operation} failed: {message}")]
    Driver {
        operation: &'static str,
        message: String,
    },
    #[error("{field} contains NUL byte")]
    HeaderContainsNul { field: &'static str },
    #[error("request body is too large: {len} bytes")]
    BodyTooLarge { len: usize },
    #[error("request timeout is too large: {timeout_ms} ms")]
    TimeoutTooLarge { timeout_ms: u32 },
    #[error("HTTP status code is outside u16 range: {status}")]
    InvalidStatusCode { status: i32 },
}

impl HttpRequestFailure {
    pub fn transport(message: impl Into<String>) -> Self {
        Self::Transport(message.into())
    }

    pub fn driver(operation: &'static str, message: impl Into<String>) -> Self {
        Self::Driver {
            operation,
            message: message.into(),
        }
    }
}

/// Rust-native HTTP transport failure.
///
/// `esp_err_t` mapping for the C ABI lives in `claw_capi::errmap::http_esp_err`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    #[error("HTTP request aborted by caller")]
    Aborted,
    #[error("invalid URL")]
    InvalidUrl,
    #[error("request body contains NUL byte")]
    InvalidBody,
    #[error("failed to create HTTP client")]
    ClientInitFailed,
    #[error("HTTP request failed: {0}")]
    RequestFailed(#[source] HttpRequestFailure),
    /// Non-success response; message matches C `parse_error_message_body` shape.
    #[error("{message}")]
    UnexpectedStatus {
        status: HttpStatusCode,
        message: String,
    },
}

pub mod blocking;

/// Boxed future returned by [`ClawHttp::post_json`].
///
/// Boxed (instead of an `async fn` in the trait) so [`ClawHttp`] stays object
/// safe. The future borrows `self` mutably, the request, and the cancellation
/// token, so it cannot outlive any of them.
pub type HttpResponseFuture<'a> =
    Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + 'a>>;

/// Cooperative cancellation token for [`ClawHttp`].
///
/// A thin, `Copy` wrapper over a caller-owned abort flag. Implementations check
/// the token cooperatively: a non-blocking driver should check it before each
/// transfer step and cancel its in-flight request when cancellation is observed.
/// Set the flag from any context (another task, an interrupt) to request
/// cancellation.
///
/// `Cancel` exists so the async seam never exposes a bare `&AtomicBool`; it is
/// the home for the abort signal and any future awaitable extension.
///
/// # Examples
///
/// ```
/// use claw_interface::Cancel;
/// use core::sync::atomic::{AtomicBool, Ordering};
///
/// let flag = AtomicBool::new(false);
/// let cancel = Cancel::new(&flag);
/// assert!(!cancel.is_cancelled());
///
/// flag.store(true, Ordering::Relaxed); // request cancellation from any context
/// assert!(cancel.is_cancelled());
///
/// // A token that never cancels, for callers that do not cancel:
/// assert!(!Cancel::never().is_cancelled());
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Cancel<'a>(&'a AtomicBool);

impl<'a> Cancel<'a> {
    /// Wrap a caller-owned abort flag.
    pub fn new(flag: &'a AtomicBool) -> Self {
        Self(flag)
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl Cancel<'static> {
    /// A token that never cancels — for callers/tests that do not cancel.
    pub fn never() -> Self {
        static NEVER: AtomicBool = AtomicBool::new(false);
        Cancel(&NEVER)
    }
}

/// HTTP transport driven by a cooperative executor instead of blocking the
/// calling task for the whole request.
///
/// On ESP-IDF this maps onto `esp_http_client`'s non-blocking mode
/// (`config.is_async = true`): each poll runs one `esp_http_client_perform`
/// step and yields (`Poll::Pending`) while the call reports
/// `ESP_ERR_HTTP_EAGAIN`, letting other tasks run between steps.
///
/// Thread-safety is intentionally not a property of this base trait. Host
/// transports such as `reqwest::Client` may be `Send + Sync`, while embedded
/// transports may be task-local because they advance raw driver handles across
/// polls. Require `Send`/`Sync` at the caller boundary that actually crosses
/// threads.
pub trait ClawHttp {
    /// Async JSON POST.
    ///
    /// Implementations must observe `cancel` cooperatively and return
    /// [`HttpError::Aborted`] when cancellation wins. A non-blocking driver
    /// should release any in-flight request state when the returned future is
    /// dropped.
    fn post_json<'a>(
        &'a mut self,
        request: &'a HttpJsonRequest<'a>,
        cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a>;

    /// Async JSON GET for read-only capability clients.
    ///
    /// Implementations that only support POST keep the default rejection.
    /// Callers should surface it as a capability/tool invocation error rather
    /// than silently changing methods.
    fn get_json<'a>(
        &'a mut self,
        _request: &'a HttpGetRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async {
            Err(HttpError::RequestFailed(HttpRequestFailure::driver(
                "http get",
                "transport does not support async HTTP GET",
            )))
        })
    }
}

/// A streaming HTTP transport: POST a JSON body and read the response body as a
/// stream of raw chunks (for Server-Sent Events).
///
/// Kept separate from [`ClawHttp`] so the one-shot seam stays object-safe. This
/// seam is always used generically (`H: StreamingHttp`), never as `dyn`, which
/// lets it carry a lifetime-indexed GAT: **each transport names a body-stream
/// type that keeps the transport mutably borrowed until that stream reaches EOF
/// or is dropped**. This statically enforces one in-flight operation per
/// transport. The device driver uses a concrete `poll_next` over its existing
/// `esp_http_client`; only host transports whose stream type is unnameable
/// (e.g. `reqwest::Response::bytes_stream`) pay for boxing.
///
/// The stream keeps the mutable transport borrow alive, so a second request
/// cannot start before the first stream is dropped:
///
/// ```compile_fail
/// use claw_interface::http::{Cancel, HttpError, HttpJsonRequest, StreamingHttp};
///
/// async fn overlapping<'r, H: StreamingHttp>(
///     http: &mut H,
///     request: &'r HttpJsonRequest<'r>,
/// ) -> Result<(), HttpError> {
///     let (_, first) = http
///         .post_json_streaming(request, Cancel::never())
///         .await?;
///     let _second = http
///         .post_json_streaming(request, Cancel::never())
///         .await?;
///     drop(first);
///     Ok(())
/// }
/// ```
pub trait StreamingHttp {
    /// This transport's response-body chunk stream. The `'a` lifetime carries
    /// the exclusive borrow of the transport for the entire response body. A
    /// device implementation can therefore read from its existing client
    /// handle without allocating a second HTTP client. Cancelling body
    /// streaming is done by **dropping** the stream or triggering the supplied
    /// [`Cancel`] token.
    ///
    /// `Unpin` keeps consumers pin-free; a host stream whose concrete type is
    /// `!Unpin` (e.g. `reqwest`'s) boxes into `Pin<Box<dyn Stream>>`, which is
    /// `Unpin` — and it had to box its unnameable type anyway.
    type ByteStream<'a>: Stream<Item = Result<Vec<u8>, HttpError>> + Unpin + 'a
    where
        Self: 'a;

    /// POST `request` and resolve the response status paired with a stream of raw
    /// body chunks. The status arrives first so the caller can read a non-2xx
    /// response as an error body; the stream carries only transport read errors.
    ///
    /// `cancel` covers both the send/header phase and response-body reads. Its
    /// lifetime matches the transport borrow because the returned stream keeps
    /// observing it. Dropping [`Self::ByteStream`] also cancels body streaming.
    /// The request lifetime is independent and is not retained by the stream.
    fn post_json_streaming<'a, 'r>(
        &'a mut self,
        request: &'r HttpJsonRequest<'r>,
        cancel: Cancel<'a>,
    ) -> impl Future<Output = Result<(HttpStatusCode, Self::ByteStream<'a>), HttpError>>;
}

/// Drive `future` while checking `cancel` before each poll.
///
/// This is a helper for implementations that already have a cancellable
/// in-flight future. The cancellation side is poll-bound: setting the flag does
/// not wake a parked future by itself.
#[cfg_attr(not(feature = "realhttp"), allow(dead_code))]
fn cancel_on_poll<'a>(
    mut transfer: HttpResponseFuture<'a>,
    cancel: Cancel<'a>,
) -> HttpResponseFuture<'a> {
    Box::pin(async move {
        core::future::poll_fn(move |context| {
            if cancel.is_cancelled() {
                return Poll::Ready(Err(HttpError::Aborted));
            }
            transfer.as_mut().poll(context)
        })
        .await
    })
}

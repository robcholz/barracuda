use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};

use super::{
    blocking, Cancel, ClawHttp, HttpError, HttpJsonRequest, HttpRequestFailure, HttpResponse,
    HttpResponseFuture, HttpStatusCode, Stream, StreamingHttp,
};

/// One scripted LLM round: a 200 body, or a transport-level error message.
pub type ScriptStep = Result<String, String>;

/// A response body served as a queue of raw byte chunks. Names a concrete
/// [`Stream`] type so [`ChunkedHttp`] stays allocation-light and mirrors how a
/// device transport would name its own stream (no boxing).
pub struct SliceChunks<'a> {
    chunks: VecDeque<Vec<u8>>,
    // Carry the transport borrow in the concrete type. Without this marker,
    // a caller using `ChunkedHttp` directly (rather than through a generic
    // `H: StreamingHttp`) could start another request while still holding
    // the first response stream.
    _transport: PhantomData<&'a mut ()>,
    cancel: Cancel<'a>,
    terminated: bool,
}

impl SliceChunks<'static> {
    /// A stream that yields `chunks` in order (format-agnostic).
    pub fn from_chunks(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self::from_chunks_with_cancel(chunks, Cancel::never())
    }

    /// A stream that yields `body` as a single chunk.
    pub fn once(body: Vec<u8>) -> Self {
        Self::from_chunks([body])
    }
}

impl<'a> SliceChunks<'a> {
    /// A chunk stream that cooperatively observes `cancel` on every poll.
    pub fn from_chunks_with_cancel(
        chunks: impl IntoIterator<Item = Vec<u8>>,
        cancel: Cancel<'a>,
    ) -> Self {
        Self {
            chunks: chunks.into_iter().collect(),
            _transport: PhantomData,
            cancel,
            terminated: false,
        }
    }

    /// A cancellable stream that yields `body` as a single chunk.
    pub fn once_with_cancel(body: Vec<u8>, cancel: Cancel<'a>) -> Self {
        Self::from_chunks_with_cancel([body], cancel)
    }
}

impl Stream for SliceChunks<'_> {
    type Item = Result<Vec<u8>, HttpError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.terminated {
            return Poll::Ready(None);
        }
        if self.cancel.is_cancelled() {
            self.terminated = true;
            return Poll::Ready(Some(Err(HttpError::Aborted)));
        }
        let next = self.chunks.pop_front().map(Ok);
        if next.is_none() {
            self.terminated = true;
        }
        Poll::Ready(next)
    }
}

/// Streaming [`StreamingHttp`] double: serves preset response bodies as byte
/// chunks, so tests can exercise SSE parsing across arbitrary chunk splits.
/// Panics if called more times than scripted.
///
/// The concrete mock preserves the same exclusive-stream rule as a device
/// transport:
///
/// ```compile_fail
/// use claw_interface::http::{ChunkedHttp, StreamingHttp};
/// use claw_interface::{Cancel, HttpAuth, HttpJsonRequest};
///
/// async fn overlap() {
///     let mut http = ChunkedHttp::new(["data: [DONE]\n\n"], 0);
///     let request = HttpJsonRequest {
///         url: "http://example.test",
///         body: "{}",
///         auth: HttpAuth::None,
///         timeout_ms: 1_000,
///         headers: &[],
///     };
///     let (_, first) = http
///         .post_json_streaming(&request, Cancel::never())
///         .await
///         .unwrap();
///     let _second = http
///         .post_json_streaming(&request, Cancel::never())
///         .await;
///     drop(first);
/// }
/// ```
pub struct ChunkedHttp {
    rounds: VecDeque<(HttpStatusCode, Vec<Vec<u8>>)>,
}

impl ChunkedHttp {
    /// Each body is served at HTTP 200 in `chunk_size`-byte pieces (the final
    /// piece may be shorter). `chunk_size == 0` serves each body whole.
    pub fn new(bodies: impl IntoIterator<Item = impl Into<String>>, chunk_size: usize) -> Self {
        let rounds = bodies
            .into_iter()
            .map(|body| (HttpStatusCode::OK, split_bytes(body.into(), chunk_size)))
            .collect();
        Self { rounds }
    }

    /// Full control: an explicit status and chunk list per round.
    pub fn with_rounds(rounds: impl IntoIterator<Item = (HttpStatusCode, Vec<Vec<u8>>)>) -> Self {
        Self {
            rounds: rounds.into_iter().collect(),
        }
    }

    fn serve<'a>(&'a mut self, cancel: Cancel<'a>) -> (HttpStatusCode, SliceChunks<'a>) {
        let (status, chunks) = self
            .rounds
            .pop_front()
            .expect("ChunkedHttp: LLM called more times than scripted");
        (
            status,
            SliceChunks {
                chunks: chunks.into(),
                _transport: PhantomData,
                cancel,
                terminated: false,
            },
        )
    }
}

/// `ChunkedHttp` is streaming-only; the one-shot seam rejects so it can still
/// stand in as the `H` of a `ClawApiAsync<H, _>` whose only call is `chat_stream`.
impl ClawHttp for ChunkedHttp {
    fn post_json<'a>(
        &'a mut self,
        _request: &'a HttpJsonRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async {
            Err(HttpError::RequestFailed(HttpRequestFailure::driver(
                "chunked http",
                "ChunkedHttp is streaming-only; use chat_stream",
            )))
        })
    }
}

impl StreamingHttp for ChunkedHttp {
    type ByteStream<'a>
        = SliceChunks<'a>
    where
        Self: 'a;

    async fn post_json_streaming<'a, 'r>(
        &'a mut self,
        _request: &'r HttpJsonRequest<'r>,
        cancel: Cancel<'a>,
    ) -> Result<(HttpStatusCode, SliceChunks<'a>), HttpError> {
        if cancel.is_cancelled() {
            return Err(HttpError::Aborted);
        }
        Ok(self.serve(cancel))
    }
}

fn split_bytes(body: String, chunk_size: usize) -> Vec<Vec<u8>> {
    let bytes = body.into_bytes();
    if chunk_size == 0 || bytes.is_empty() {
        return vec![bytes];
    }
    bytes.chunks(chunk_size).map(<[u8]>::to_vec).collect()
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn respond(step: ScriptStep) -> Result<HttpResponse, HttpError> {
    match step {
        Ok(body) => Ok(HttpResponse {
            status_code: HttpStatusCode::OK,
            body,
        }),
        Err(message) => Err(HttpError::RequestFailed(HttpRequestFailure::transport(
            message,
        ))),
    }
}

fn into_steps(bodies: impl IntoIterator<Item = impl Into<String>>) -> VecDeque<ScriptStep> {
    bodies.into_iter().map(|body| Ok(body.into())).collect()
}

/// Replays scripted rounds in order; panics if the LLM is called more times
/// than scripted, so an unexpected round fails loudly instead of silently.
pub struct ScriptedHttp {
    steps: Mutex<VecDeque<ScriptStep>>,
}

impl ScriptedHttp {
    /// Script of all-success bodies served in order.
    pub fn new(bodies: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            steps: Mutex::new(into_steps(bodies)),
        }
    }

    /// Script of mixed success / transport-error rounds served in order.
    pub fn with_steps(steps: impl IntoIterator<Item = ScriptStep>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
        }
    }

    /// Serve the next scripted step. Takes `&self` (state is interior-mutable)
    /// so the double works both owned and behind an `Arc`.
    fn serve(&self) -> Result<HttpResponse, HttpError> {
        let step = guard(&self.steps)
            .pop_front()
            .expect("ScriptedHttp: LLM called more times than scripted");
        respond(step)
    }
}

impl blocking::ClawHttp for ScriptedHttp {
    fn post_json(
        &mut self,
        _request: &HttpJsonRequest,
        _abort: &AtomicBool,
    ) -> Result<HttpResponse, HttpError> {
        self.serve()
    }
}

/// Share one script across clients that each *own* their transport (e.g. a
/// `ClawApi<Arc<ScriptedHttp>>`): every clone replays from the same queue.
impl blocking::ClawHttp for Arc<ScriptedHttp> {
    fn post_json(
        &mut self,
        _request: &HttpJsonRequest,
        _abort: &AtomicBool,
    ) -> Result<HttpResponse, HttpError> {
        self.serve()
    }
}

/// Like [`ScriptedHttp`] but records every request body so a test can assert
/// on the context the agent sent to the model.
pub struct CapturingHttp {
    steps: Mutex<VecDeque<ScriptStep>>,
    captured: Mutex<Vec<String>>,
}

impl CapturingHttp {
    /// Script of all-success bodies; returns an `Arc` for sharing with the LLM.
    pub fn new(bodies: impl IntoIterator<Item = impl Into<String>>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(into_steps(bodies)),
            captured: Mutex::new(Vec::new()),
        })
    }

    /// Script of mixed success / transport-error rounds.
    pub fn with_steps(steps: impl IntoIterator<Item = ScriptStep>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps.into_iter().collect()),
            captured: Mutex::new(Vec::new()),
        })
    }

    /// Raw request bodies the agent sent, in call order.
    pub fn captured(&self) -> Vec<String> {
        guard(&self.captured).clone()
    }

    /// Request bodies parsed as JSON, in call order (unparseable → `Null`).
    pub fn captured_bodies(&self) -> Vec<serde_json::Value> {
        guard(&self.captured)
            .iter()
            .map(|body| match serde_json::from_str(body) {
                Ok(value) => value,
                Err(_) => serde_json::Value::Null,
            })
            .collect()
    }

    /// Number of LLM calls made so far.
    pub fn call_count(&self) -> usize {
        guard(&self.captured).len()
    }

    /// Record `request` and serve the next scripted step. Takes `&self` (state
    /// is interior-mutable) so the double works both owned and behind an `Arc`
    /// — the latter lets a test keep a handle to assert on `captured` after the
    /// client has consumed it.
    fn serve(&self, request: &HttpJsonRequest) -> Result<HttpResponse, HttpError> {
        guard(&self.captured).push(request.body.to_string());
        let step = guard(&self.steps)
            .pop_front()
            .expect("CapturingHttp: LLM called more times than scripted");
        respond(step)
    }
}

impl blocking::ClawHttp for CapturingHttp {
    fn post_json(
        &mut self,
        request: &HttpJsonRequest,
        _abort: &AtomicBool,
    ) -> Result<HttpResponse, HttpError> {
        self.serve(request)
    }
}

/// Lets a test hold an `Arc<CapturingHttp>` for inspection while a
/// `ClawApi<Arc<CapturingHttp>>` owns a clone and drives it.
impl blocking::ClawHttp for Arc<CapturingHttp> {
    fn post_json(
        &mut self,
        request: &HttpJsonRequest,
        _abort: &AtomicBool,
    ) -> Result<HttpResponse, HttpError> {
        self.serve(request)
    }
}

/// The script every [`SharedScriptHttp::default`] in the process shares.
/// Installed by [`SharedScriptHttp::install`]; read at each construction.
///
/// Process-global (not thread-local) so a `SharedScriptHttp` built on an
/// orchestrator's drive **worker thread** replays the script a test installed
/// on its own thread. Tests that install scripts must serialize with
/// [`SharedScriptHttp::serialize`] so parallel tests do not clobber it.
static SHARED_SCRIPT: Mutex<Option<Arc<Mutex<VecDeque<ScriptStep>>>>> = Mutex::new(None);
/// Held by a test (via [`SharedScriptHttp::serialize`]) for the whole span in
/// which it owns the process-global script.
static SHARED_SCRIPT_LOCK: Mutex<()> = Mutex::new(());

fn shared_script_slot() -> std::sync::MutexGuard<'static, Option<Arc<Mutex<VecDeque<ScriptStep>>>>>
{
    SHARED_SCRIPT
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// A [`Default`]-constructible scripted transport for systems that mint their
/// own clients and choose the transport by *type* (e.g. `FsAgentFactory<F, H>`
/// / `AgentSystem` with `H = SharedScriptHttp`).
///
/// A plain [`ScriptedHttp`] can't be injected into those, because the system
/// constructs each `H::default()` internally. `SharedScriptHttp` bridges the
/// gap: a test calls [`install`](Self::install) once, then **every**
/// `SharedScriptHttp::default()` in the process shares the one script and pops
/// from it in call order — reproducing the single-shared-script behavior of an
/// injected [`ScriptedHttp`], including from a drive worker thread. Strict:
/// panics if called more times than scripted, or if no script was installed.
#[derive(Clone)]
pub struct SharedScriptHttp {
    steps: Option<Arc<Mutex<VecDeque<ScriptStep>>>>,
}

impl SharedScriptHttp {
    /// Install the script shared by every later `SharedScriptHttp::default()`
    /// in the process. Call once before building the system under test, while
    /// holding the [`serialize`](Self::serialize) guard.
    pub fn install(bodies: impl IntoIterator<Item = impl Into<String>>) {
        let steps = Arc::new(Mutex::new(into_steps(bodies)));
        *shared_script_slot() = Some(steps);
    }

    /// Drop the installed script (so a later `default()` with no script fails
    /// loudly rather than replaying a stale one).
    pub fn clear() {
        *shared_script_slot() = None;
    }

    /// Serialize access to the process-global script. Hold the returned guard
    /// for the whole test body (install → submit → drain) so parallel tests
    /// never share one another's script.
    pub fn serialize() -> std::sync::MutexGuard<'static, ()> {
        SHARED_SCRIPT_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Default for SharedScriptHttp {
    fn default() -> Self {
        Self {
            steps: shared_script_slot().clone(),
        }
    }
}

impl blocking::ClawHttp for SharedScriptHttp {
    fn post_json(
        &mut self,
        _request: &HttpJsonRequest,
        _abort: &AtomicBool,
    ) -> Result<HttpResponse, HttpError> {
        let steps = self
            .steps
            .as_ref()
            .expect("SharedScriptHttp: no script installed; call SharedScriptHttp::install(..)");
        let step = guard(steps)
            .pop_front()
            .expect("SharedScriptHttp: LLM called more times than scripted");
        respond(step)
    }
}

/// Always fails the LLM round with a transport error.
pub struct FailingHttp;

impl blocking::ClawHttp for FailingHttp {
    fn post_json(
        &mut self,
        _request: &HttpJsonRequest,
        _abort: &AtomicBool,
    ) -> Result<HttpResponse, HttpError> {
        Err(HttpError::RequestFailed(HttpRequestFailure::transport(
            "simulated failure",
        )))
    }
}

/// Returns a no-op transport error; for wiring where the LLM is never driven.
pub struct NoopHttp;

impl blocking::ClawHttp for NoopHttp {
    fn post_json(
        &mut self,
        _request: &HttpJsonRequest,
        _abort: &AtomicBool,
    ) -> Result<HttpResponse, HttpError> {
        Err(HttpError::RequestFailed(HttpRequestFailure::transport(
            "noop",
        )))
    }
}

/// Panics if called — for tests that must never reach the LLM.
pub struct NeverHttp;

impl blocking::ClawHttp for NeverHttp {
    fn post_json(
        &mut self,
        _request: &HttpJsonRequest,
        _abort: &AtomicBool,
    ) -> Result<HttpResponse, HttpError> {
        panic!("NeverHttp: the LLM must not be called in this test");
    }
}

/// Adapts any blocking [`blocking::ClawHttp`] into an async HTTP transport by
/// running the request to completion in a single poll.
///
/// Intended for the host (CLIs, tests) where the blocking transports already
/// exist and a real cooperative executor is unnecessary. It is **not**
/// appropriate on-device: the wrapped call blocks the polling task for the
/// whole request.
pub struct BlockingHttpAdapter<T>(T);

impl<T> BlockingHttpAdapter<T> {
    /// Wrap a blocking [`blocking::ClawHttp`] for the async seam.
    pub fn new(inner: T) -> Self {
        Self(inner)
    }
}

impl<T: Default> Default for BlockingHttpAdapter<T> {
    fn default() -> Self {
        Self(T::default())
    }
}

impl<T: blocking::ClawHttp> ClawHttp for BlockingHttpAdapter<T> {
    fn post_json<'a>(
        &'a mut self,
        request: &'a HttpJsonRequest<'a>,
        cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(HttpError::Aborted);
            }
            let never = AtomicBool::new(false);
            self.0.post_json(request, &never)
        })
    }
}

/// Yields to the executor exactly once, then resolves.
async fn yield_once() {
    struct YieldOnce(bool);
    impl Future for YieldOnce {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                context.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
    YieldOnce(false).await
}

/// Adapts a blocking transport into async HTTP after yielding `yields` times.
///
/// This models the multi-poll shape of a non-blocking device transport for
/// host tests. The final transfer remains blocking, so this adapter is not
/// appropriate on-device.
pub struct YieldingHttpAdapter<T> {
    inner: T,
    yields: u32,
}

impl<T> YieldingHttpAdapter<T> {
    /// Wrap `inner`, yielding to the executor `yields` times before resolving.
    pub fn new(inner: T, yields: u32) -> Self {
        Self { inner, yields }
    }
}

impl<T: blocking::ClawHttp> ClawHttp for YieldingHttpAdapter<T> {
    fn post_json<'a>(
        &'a mut self,
        request: &'a HttpJsonRequest<'a>,
        cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async move {
            for _ in 0..self.yields {
                if cancel.is_cancelled() {
                    return Err(HttpError::Aborted);
                }
                yield_once().await;
            }
            if cancel.is_cancelled() {
                return Err(HttpError::Aborted);
            }
            let never = AtomicBool::new(false);
            let result = self.inner.post_json(request, &never);
            if cancel.is_cancelled() {
                return Err(HttpError::Aborted);
            }
            result
        })
    }
}

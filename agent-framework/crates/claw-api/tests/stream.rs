//! End-to-end streaming tests: drive [`ClawApiAsync::chat_stream`] over the
//! `ChunkedHttp` double, which serves an SSE body in small byte chunks so the
//! parser is exercised across arbitrary frame/codepoint splits.

use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use claw_api::{
    BackendKind, ChatError, ChatRequest, ChatStreamEvent, ClawApiAsync, ClawApiConfig, RetryPolicy,
    ToolCall,
};
use claw_interface::http::{
    ChunkedHttp, ClawHttp, HttpError, HttpJsonRequest, HttpRequestFailure, HttpResponseFuture,
    HttpStatusCode, StreamingHttp,
};
use claw_interface::{Cancel, ClawTimer, ImmediateTimer, SleepOutcome, TimerFuture};
use claw_utils::stream::StreamPart;
use futures_core::Stream;
use futures_lite::future::block_on;
use futures_lite::StreamExt;
use serde_json::json;

fn config(backend: BackendKind) -> ClawApiConfig {
    ClawApiConfig::new(backend, "key", "model", "http://example.test/v1")
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn transport_error(message: &str) -> HttpError {
    HttpError::RequestFailed(HttpRequestFailure::transport(message))
}

enum StreamingRound {
    OpenError(HttpError),
    Response {
        status: HttpStatusCode,
        items: VecDeque<Result<Vec<u8>, HttpError>>,
    },
}

impl StreamingRound {
    fn ok(items: impl IntoIterator<Item = Result<Vec<u8>, HttpError>>) -> Self {
        Self::Response {
            status: HttpStatusCode::OK,
            items: items.into_iter().collect(),
        }
    }
}

struct RetryBody<'a> {
    items: VecDeque<Result<Vec<u8>, HttpError>>,
    cancel: Cancel<'a>,
    _transport: PhantomData<&'a mut ()>,
}

impl Stream for RetryBody<'_> {
    type Item = Result<Vec<u8>, HttpError>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.cancel.is_cancelled() {
            return Poll::Ready(Some(Err(HttpError::Aborted)));
        }
        Poll::Ready(self.items.pop_front())
    }
}

struct RetryHttp {
    rounds: VecDeque<StreamingRound>,
    attempts: Arc<Mutex<u32>>,
}

impl RetryHttp {
    fn new(rounds: impl IntoIterator<Item = StreamingRound>) -> (Self, Arc<Mutex<u32>>) {
        let attempts = Arc::new(Mutex::new(0));
        (
            Self {
                rounds: rounds.into_iter().collect(),
                attempts: Arc::clone(&attempts),
            },
            attempts,
        )
    }
}

impl ClawHttp for RetryHttp {
    fn post_json<'a>(
        &'a mut self,
        _request: &'a HttpJsonRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async {
            Err(HttpError::RequestFailed(HttpRequestFailure::driver(
                "retry http",
                "RetryHttp is streaming-only",
            )))
        })
    }
}

impl StreamingHttp for RetryHttp {
    type ByteStream<'a>
        = RetryBody<'a>
    where
        Self: 'a;

    async fn post_json_streaming<'a, 'r>(
        &'a mut self,
        _request: &'r HttpJsonRequest<'r>,
        cancel: Cancel<'a>,
    ) -> Result<(HttpStatusCode, Self::ByteStream<'a>), HttpError> {
        {
            let mut attempts = lock(&self.attempts);
            *attempts = attempts.saturating_add(1);
        }
        match self
            .rounds
            .pop_front()
            .expect("RetryHttp called more times than scripted")
        {
            StreamingRound::OpenError(error) => Err(error),
            StreamingRound::Response { status, items } => Ok((
                status,
                RetryBody {
                    items,
                    cancel,
                    _transport: PhantomData,
                },
            )),
        }
    }
}

#[derive(Clone)]
struct RecordingTimer {
    sleeps: Arc<Mutex<Vec<Duration>>>,
    outcome: SleepOutcome,
}

impl RecordingTimer {
    fn completed() -> (Self, Arc<Mutex<Vec<Duration>>>) {
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                sleeps: Arc::clone(&sleeps),
                outcome: SleepOutcome::Completed,
            },
            sleeps,
        )
    }

    fn cancelled() -> Self {
        Self {
            sleeps: Arc::new(Mutex::new(Vec::new())),
            outcome: SleepOutcome::Cancelled,
        }
    }
}

impl ClawTimer for RecordingTimer {
    fn sleep<'a>(&'a mut self, duration: Duration, _cancel: Cancel<'a>) -> TimerFuture<'a> {
        lock(&self.sleeps).push(duration);
        let outcome = self.outcome;
        Box::pin(async move { outcome })
    }
}

fn openai_done(text: &str) -> Vec<u8> {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\ndata: [DONE]\n\n")
        .into_bytes()
}

/// Drive a `chat_stream` to completion and collect only its semantic events.
fn run(backend: BackendKind, sse_body: &str, chunk_size: usize) -> Vec<ChatStreamEvent> {
    let http = ChunkedHttp::new([sse_body.to_string()], chunk_size);
    let mut rt = ClawApiAsync::<ChunkedHttp, ImmediateTimer>::new(http, ImmediateTimer);
    rt.set_config(config(backend)).unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages);

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        let mut deltas = Vec::new();
        while let Some(event) = stream.next().await {
            deltas.push(event.expect("stream event"));
        }
        deltas
    })
}

#[test]
fn openai_streams_semantic_events_without_assembling_a_response() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"th\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"ink\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"ping\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    // 7-byte chunks split SSE frames and JSON tokens mid-way on purpose.
    let deltas = run(BackendKind::OpenAiCompatible, sse, 7);

    assert_eq!(
        deltas,
        vec![
            ChatStreamEvent::Reasoning(StreamPart::Delta("th".to_string())),
            ChatStreamEvent::Reasoning(StreamPart::Delta("ink".to_string())),
            ChatStreamEvent::Reasoning(StreamPart::End),
            ChatStreamEvent::Output(StreamPart::Delta("Hel".to_string())),
            ChatStreamEvent::Output(StreamPart::Delta("lo".to_string())),
            ChatStreamEvent::Output(StreamPart::End),
            ChatStreamEvent::ToolCalls(StreamPart::Delta(ToolCall {
                id: "call_1".to_string(),
                name: "ping".to_string(),
                arguments_json: "{}".to_string(),
            })),
            ChatStreamEvent::ToolCalls(StreamPart::End),
        ]
    );
}

#[test]
fn anthropic_streams_semantic_events_without_assembling_a_response() {
    let sse = concat!(
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"ping\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let deltas = run(BackendKind::AnthropicCompatible, sse, 9);

    assert_eq!(
        deltas,
        vec![
            ChatStreamEvent::Reasoning(StreamPart::End),
            ChatStreamEvent::Output(StreamPart::Delta("Hi".to_string())),
            ChatStreamEvent::Output(StreamPart::End),
            ChatStreamEvent::ToolCalls(StreamPart::Delta(ToolCall {
                id: "toolu_1".to_string(),
                name: "ping".to_string(),
                arguments_json: "{}".to_string(),
            })),
            ChatStreamEvent::ToolCalls(StreamPart::End),
        ]
    );
}

#[test]
fn premature_eof_is_yielded_as_a_stream_error() {
    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
    let http = ChunkedHttp::new([sse], 64);
    let mut rt = ClawApiAsync::<ChunkedHttp, ImmediateTimer>::new(http, ImmediateTimer);
    rt.set_config(config(BackendKind::OpenAiCompatible))
        .unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages);

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        assert_eq!(
            stream.next().await,
            Some(Ok(ChatStreamEvent::Reasoning(StreamPart::End)))
        );
        assert_eq!(
            stream.next().await,
            Some(Ok(ChatStreamEvent::Output(StreamPart::Delta(
                "partial".to_string()
            ))))
        );
        assert_eq!(
            stream.next().await,
            Some(Err(ChatError::truncated_stream()))
        );
        assert_eq!(stream.next().await, None);
    });
}

#[test]
fn streaming_abort_remains_active_after_headers() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"second\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let http = ChunkedHttp::new([sse], 64);
    let mut rt = ClawApiAsync::<ChunkedHttp, ImmediateTimer>::new(http, ImmediateTimer);
    rt.set_config(config(BackendKind::OpenAiCompatible))
        .unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages);

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        assert_eq!(
            stream.next().await,
            Some(Ok(ChatStreamEvent::Reasoning(StreamPart::End)))
        );
        assert!(matches!(
            stream.next().await,
            Some(Ok(ChatStreamEvent::Output(StreamPart::Delta(text)))) if text == "first"
        ));
        abort.store(true, Ordering::Relaxed);
        let error = stream
            .next()
            .await
            .expect("abort item")
            .expect_err("aborted");
        assert!(error.is_aborted(), "{error}");
    });
}

#[test]
fn streaming_open_failure_retries_before_returning_the_stream() {
    let (http, attempts) = RetryHttp::new([
        StreamingRound::OpenError(transport_error("dns unavailable")),
        StreamingRound::ok([Ok(openai_done("ok"))]),
    ]);
    let (timer, sleeps) = RecordingTimer::completed();
    let mut rt = ClawApiAsync::new(http, timer);
    rt.set_config(config(BackendKind::OpenAiCompatible))
        .unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages).with_retry(RetryPolicy::fixed(2, 25));

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        let mut output = String::new();
        while let Some(item) = stream.next().await {
            if let ChatStreamEvent::Output(StreamPart::Delta(delta)) = item.unwrap() {
                output.push_str(&delta);
            }
        }
        assert_eq!(output, "ok");
    });

    assert_eq!(*lock(&attempts), 2);
    assert_eq!(lock(&sleeps).as_slice(), &[Duration::from_millis(25)]);
}

#[test]
fn streaming_body_failure_retries_before_first_event() {
    let (http, attempts) = RetryHttp::new([
        StreamingRound::ok([Err(transport_error("connection reset"))]),
        StreamingRound::ok([Ok(openai_done("recovered"))]),
    ]);
    let (timer, sleeps) = RecordingTimer::completed();
    let mut rt = ClawApiAsync::new(http, timer);
    rt.set_config(config(BackendKind::OpenAiCompatible))
        .unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages).with_retry(RetryPolicy::fixed(2, 40));

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        let mut output = String::new();
        while let Some(item) = stream.next().await {
            if let ChatStreamEvent::Output(StreamPart::Delta(delta)) = item.unwrap() {
                output.push_str(&delta);
            }
        }
        assert_eq!(output, "recovered");
    });

    assert_eq!(*lock(&attempts), 2);
    assert_eq!(lock(&sleeps).as_slice(), &[Duration::from_millis(40)]);
}

#[test]
fn streaming_premature_eof_retries_before_first_event() {
    let (http, attempts) = RetryHttp::new([
        StreamingRound::ok(std::iter::empty()),
        StreamingRound::ok([Ok(openai_done("recovered"))]),
    ]);
    let (timer, sleeps) = RecordingTimer::completed();
    let mut rt = ClawApiAsync::new(http, timer);
    rt.set_config(config(BackendKind::OpenAiCompatible))
        .unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages).with_retry(RetryPolicy::fixed(1, 30));

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        let mut output = String::new();
        while let Some(item) = stream.next().await {
            if let ChatStreamEvent::Output(StreamPart::Delta(delta)) = item.unwrap() {
                output.push_str(&delta);
            }
        }
        assert_eq!(output, "recovered");
    });

    assert_eq!(*lock(&attempts), 2);
    assert_eq!(lock(&sleeps).as_slice(), &[Duration::from_millis(30)]);
}

#[test]
fn streaming_body_failure_after_first_event_is_not_retried() {
    let first = b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n".to_vec();
    let (http, attempts) = RetryHttp::new([
        StreamingRound::ok([
            Ok(first),
            Err(transport_error("connection reset after output")),
        ]),
        StreamingRound::ok([Ok(openai_done("duplicate"))]),
    ]);
    let (timer, sleeps) = RecordingTimer::completed();
    let mut rt = ClawApiAsync::new(http, timer);
    rt.set_config(config(BackendKind::OpenAiCompatible))
        .unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages).with_retry(RetryPolicy::fixed(2, 10));

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        assert_eq!(
            stream.next().await,
            Some(Ok(ChatStreamEvent::Reasoning(StreamPart::End)))
        );
        assert_eq!(
            stream.next().await,
            Some(Ok(ChatStreamEvent::Output(StreamPart::Delta(
                "partial".to_string()
            ))))
        );
        let error = stream
            .next()
            .await
            .expect("stream error")
            .expect_err("body failure");
        assert!(error.is_retryable(), "{error}");
        assert_eq!(stream.next().await, None);
    });

    assert_eq!(*lock(&attempts), 1);
    assert!(lock(&sleeps).is_empty());
}

#[test]
fn streaming_retry_cancelled_during_backoff_returns_abort() {
    let (http, attempts) = RetryHttp::new([
        StreamingRound::OpenError(transport_error("dns unavailable")),
        StreamingRound::ok([Ok(openai_done("must not run"))]),
    ]);
    let mut rt = ClawApiAsync::new(http, RecordingTimer::cancelled());
    rt.set_config(config(BackendKind::OpenAiCompatible))
        .unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages).with_retry(RetryPolicy::fixed(2, 10));

    let error = match block_on(rt.chat_stream(&request, Cancel::new(&abort))) {
        Ok(_) => panic!("cancelled backoff unexpectedly opened a stream"),
        Err(error) => error,
    };
    assert!(error.is_aborted(), "{error}");
    assert_eq!(*lock(&attempts), 1);
}

#[test]
fn streaming_server_error_retries_before_opening() {
    let (http, attempts) = RetryHttp::new([
        StreamingRound::Response {
            status: HttpStatusCode::new(500),
            items: [Ok(b"temporary".to_vec())].into_iter().collect(),
        },
        StreamingRound::ok([Ok(openai_done("ok"))]),
    ]);
    let (timer, _sleeps) = RecordingTimer::completed();
    let mut rt = ClawApiAsync::new(http, timer);
    rt.set_config(config(BackendKind::OpenAiCompatible))
        .unwrap();
    let abort = AtomicBool::new(false);
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let request = ChatRequest::new("sys", &messages).with_retry(RetryPolicy::fixed(1, 0));

    block_on(async {
        let mut stream = rt.chat_stream(&request, Cancel::new(&abort)).await.unwrap();
        while let Some(item) = stream.next().await {
            item.unwrap();
        }
    });

    assert_eq!(*lock(&attempts), 2);
}

//! [`ChatStream`]: the streaming counterpart of [`crate::ClawApiAsync::chat`].
//!
//! Wraps a transport byte stream ([`StreamingHttp::ByteStream`](claw_interface::http::StreamingHttp::ByteStream))
//! with a provider SSE parser and yields ordered [`ChatStreamEvent`]s as they
//! arrive.

use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::VecDeque;

use futures_core::Stream;
use futures_lite::StreamExt;

use claw_interface::http::HttpError;

use crate::backends::sse::ProviderSse;
use crate::errors::{ChatError, ClawApiError};
use crate::types::ChatStreamEvent;

/// A streaming chat completion.
///
/// Implements [`Stream`] over `Result<ChatStreamEvent, ChatError>`. Reasoning,
/// output, and tool-call logical streams each carry
/// [`StreamPart`](claw_utils::stream::StreamPart) values and an explicit `End`.
/// Normal provider completion then yields `None`; parse, transport,
/// cancellation, and premature EOF failures are yielded as an `Err` item before
/// the stream ends. The request's cancellation token remains active during body
/// reads; dropping the stream cancels them as well.
///
/// `S` is the transport's byte stream and retains that transport's exclusive
/// mutable borrow; it (and therefore `ChatStream`) is `Unpin`, so no pinning
/// gymnastics are needed at the call site.
pub struct ChatStream<S> {
    bytes: S,
    /// `None` once the stream has completed or yielded a terminal error.
    parser: Option<ProviderSse>,
    queue: VecDeque<Result<ChatStreamEvent, ChatError>>,
}

impl<S> ChatStream<S> {
    pub(crate) fn new(bytes: S, parser: ProviderSse) -> Self {
        Self {
            bytes,
            parser: Some(parser),
            queue: VecDeque::new(),
        }
    }
}

impl<S> Stream for ChatStream<S>
where
    S: Stream<Item = Result<Vec<u8>, HttpError>> + Unpin,
{
    type Item = Result<ChatStreamEvent, ChatError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // ChatStream is Unpin (all fields are), so project by plain &mut.
        let this = self.get_mut();
        loop {
            if let Some(item) = this.queue.pop_front() {
                return Poll::Ready(Some(item));
            }
            if this.parser.is_none() {
                return Poll::Ready(None);
            }
            match Pin::new(&mut this.bytes).poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    let mut deltas = Vec::new();
                    let Some(parser) = this.parser.as_mut() else {
                        return Poll::Ready(None);
                    };
                    let result = parser.push(&chunk, &mut deltas);
                    let done = parser.is_done();
                    this.queue.extend(deltas.into_iter().map(Ok));
                    if let Err(error) = result {
                        this.parser = None;
                        this.queue.push_back(Err(error));
                    } else if done {
                        this.parser = None;
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    this.parser = None;
                    return Poll::Ready(Some(Err(read_error(error))));
                }
                Poll::Ready(None) => {
                    this.parser = None;
                    return Poll::Ready(Some(Err(ChatError::truncated_stream())));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// A transport read error mid-body: the stream already started, so this is a
/// permanent (non-retryable) transport failure rather than a connect error.
fn read_error(error: HttpError) -> ChatError {
    ClawApiError::Transport(error.to_string()).into()
}

/// Drain a byte stream to a UTF-8 string. Used to read a non-2xx error body
/// before failing a streaming request.
pub(crate) async fn drain_body<S>(mut stream: S) -> Result<String, HttpError>
where
    S: Stream<Item = Result<Vec<u8>, HttpError>> + Unpin,
{
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

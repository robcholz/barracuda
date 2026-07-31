//! [`ChatStream`]: the streaming counterpart of [`crate::ClawApiAsync::chat`].
//!
//! The public stream hides connection attempts and retry backoff. Each attempt
//! uses a private provider stream that parses one transport byte stream into
//! ordered [`ChatStreamEvent`]s.

use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::VecDeque;

use futures_core::Stream;
use futures_lite::StreamExt;

use claw_interface::http::HttpError;

use crate::backends::shared::map_http_error;
use crate::backends::sse::ProviderSse;
use crate::errors::{ChatError, ClawApiError};
use crate::types::ChatStreamEvent;

/// A streaming chat completion.
///
/// Implements [`Stream`] over `Result<ChatStreamEvent, ChatError>`. Reasoning,
/// output, and tool-call logical streams each carry
/// [`StreamPart`](claw_utils::stream::StreamPart) values and an explicit `End`.
/// Normal provider completion then yields `None`; final parse, transport,
/// cancellation, and premature EOF failures are yielded as an `Err` item before
/// the stream ends. Transient failures before the first event are retried
/// internally according to the request's [`RetryPolicy`](crate::RetryPolicy).
/// Once an event has been yielded, replay is no longer safe and failures are
/// terminal. Dropping the stream cancels the active response body.
pub struct ChatStream<'a> {
    driver: Driver<'a>,
}

pub(crate) type Driver<'a> = Pin<Box<dyn Stream<Item = DriverItem> + 'a>>;

pub(crate) enum DriverItem {
    Opened,
    Event(Result<ChatStreamEvent, ChatError>),
}

impl<'a> ChatStream<'a> {
    pub(crate) async fn open(mut driver: Driver<'a>) -> Result<Self, ChatError> {
        match driver.next().await {
            Some(DriverItem::Opened) => Ok(Self { driver }),
            Some(DriverItem::Event(Err(error))) => Err(error),
            Some(DriverItem::Event(Ok(_))) | None => Err(ChatError::Api(ClawApiError::ApiError(
                "stream driver ended before opening",
            ))),
        }
    }
}

impl Stream for ChatStream<'_> {
    type Item = Result<ChatStreamEvent, ChatError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match this.driver.as_mut().poll_next(cx) {
                Poll::Ready(Some(DriverItem::Opened)) => continue,
                Poll::Ready(Some(DriverItem::Event(item))) => return Poll::Ready(Some(item)),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// One provider response attempt over one transport byte stream.
pub(crate) struct ProviderStream<S> {
    bytes: S,
    /// `None` once the stream has completed or yielded a terminal error.
    parser: Option<ProviderSse>,
    queue: VecDeque<Result<ChatStreamEvent, ChatError>>,
}

impl<S> ProviderStream<S> {
    pub(crate) fn new(bytes: S, parser: ProviderSse) -> Self {
        Self {
            bytes,
            parser: Some(parser),
            queue: VecDeque::new(),
        }
    }
}

impl<S> Stream for ProviderStream<S>
where
    S: Stream<Item = Result<Vec<u8>, HttpError>> + Unpin,
{
    type Item = Result<ChatStreamEvent, ChatError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // ProviderStream is Unpin (all fields are), so project by plain &mut.
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

/// Preserve the transport's transient/permanent classification. The outer
/// stream driver separately decides whether replay is still safe.
fn read_error(error: HttpError) -> ChatError {
    map_http_error(error).into()
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

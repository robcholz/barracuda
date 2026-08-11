//! [`ChatStream`]: the streaming counterpart of [`crate::ClawApi::chat`].
//!
//! The public stream hides connection attempts and retry backoff. Each attempt
//! uses a private provider stream that parses one transport byte stream into
//! ordered [`ChatStreamEvent`]s.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::pin::Pin;
use core::task::{Context, Poll};

use futures_core::Stream;
use futures_lite::StreamExt;
use sseer::errors::EventStreamError;
use sseer::EventStream;

use crate::backends::shared::map_net_error;
use crate::backends::sse::ProviderSse;
use crate::errors::{ChatError, ClawApiError};
use crate::transport::{Error as NetError, ResponsePart};
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
    events: EventStream<ResponseDataStream<S>>,
    /// `None` after provider completion or a terminal parser error.
    parser: Option<ProviderSse>,
    /// Provider completion was observed; drain the remaining HTTP framing so
    /// reqwless can safely reuse the connection.
    drain_after_done: bool,
    queue: VecDeque<Result<ChatStreamEvent, ChatError>>,
}

impl<S> ProviderStream<S> {
    pub(crate) fn new(bytes: S, parser: ProviderSse) -> Self {
        Self {
            events: EventStream::new(ResponseDataStream { inner: bytes }),
            parser: Some(parser),
            drain_after_done: false,
            queue: VecDeque::new(),
        }
    }
}

impl<S> Stream for ProviderStream<S>
where
    S: Stream<Item = Result<ResponsePart, NetError>> + Unpin,
{
    type Item = Result<ChatStreamEvent, ChatError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // ProviderStream is Unpin (all fields are), so project by plain &mut.
        let this = self.get_mut();
        loop {
            if let Some(item) = this.queue.pop_front() {
                return Poll::Ready(Some(item));
            }
            if this.parser.is_none() && !this.drain_after_done {
                return Poll::Ready(None);
            }
            match Pin::new(&mut this.events).poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => {
                    if this.drain_after_done {
                        continue;
                    }
                    let mut deltas = Vec::new();
                    let Some(parser) = this.parser.as_mut() else {
                        return Poll::Ready(None);
                    };
                    let result = parser.process_data(&event.data, &mut deltas);
                    let done = parser.is_done();
                    this.queue.extend(deltas.into_iter().map(Ok));
                    if let Err(error) = result {
                        this.parser = None;
                        this.queue.push_back(Err(error));
                    } else if done {
                        this.parser = None;
                        this.drain_after_done = true;
                    }
                }
                Poll::Ready(Some(Err(EventStreamError::Transport(error)))) => {
                    this.parser = None;
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(Some(Err(EventStreamError::Utf8Error(_)))) => {
                    this.parser = None;
                    return Poll::Ready(Some(Err(ClawApiError::Parse.into())));
                }
                Poll::Ready(None) => {
                    if this.drain_after_done {
                        this.drain_after_done = false;
                        return Poll::Ready(None);
                    }
                    this.parser = None;
                    return Poll::Ready(Some(Err(ChatError::truncated_stream())));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

struct ResponseDataStream<S> {
    inner: S,
}

impl<S> Stream for ResponseDataStream<S>
where
    S: Stream<Item = Result<ResponsePart, NetError>> + Unpin,
{
    type Item = Result<Vec<u8>, ChatError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(context) {
            Poll::Ready(Some(Ok(ResponsePart::Data(chunk)))) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(Some(Ok(ResponsePart::Head(_)))) => Poll::Ready(Some(Err(ChatError::Api(
                ClawApiError::ApiError("HTTP response head was emitted twice"),
            )))),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(read_error(error)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Preserve the transport's transient/permanent classification. The outer
/// stream driver separately decides whether replay is still safe.
fn read_error(error: NetError) -> ChatError {
    map_net_error(error).into()
}

/// Drain a byte stream to a UTF-8 string. Used to read a non-2xx error body
/// before failing a streaming request.
pub(crate) async fn drain_body<S>(mut stream: S) -> Result<String, NetError>
where
    S: Stream<Item = Result<ResponsePart, NetError>> + Unpin,
{
    let mut buf = Vec::new();
    while let Some(part) = stream.next().await {
        match part? {
            ResponsePart::Head(_) => return Err(NetError::Codec),
            ResponsePart::Data(chunk) => buf.extend_from_slice(&chunk),
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

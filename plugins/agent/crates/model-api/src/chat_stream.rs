//! [`ChatStream`]: the streaming counterpart of [`crate::ModelApi::chat`].
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

use eventsource_stream::{EventStream, EventStreamError};
use futures_core::Stream;
use futures_lite::StreamExt;

use crate::backends::sse::ProviderSse;
use crate::errors::Error;
use crate::transport::ResponsePart;
use crate::types::ChatStreamEvent;

/// A streaming chat completion.
///
/// Implements [`Stream`] over `Result<ChatStreamEvent, Error>`. Reasoning,
/// output, and tool-call logical streams each carry
/// [`StreamPart`](barracuda_runtime_utils::stream::StreamPart) values and an explicit `End`.
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
type ByteStream<'a> = Pin<Box<dyn Stream<Item = Result<Vec<u8>, Error>> + 'a>>;

pub(crate) enum DriverItem {
    Opened,
    Event(Result<ChatStreamEvent, Error>),
}

impl<'a> ChatStream<'a> {
    pub(crate) async fn open(mut driver: Driver<'a>) -> Result<Self, Error> {
        match driver.next().await {
            Some(DriverItem::Opened) => Ok(Self { driver }),
            Some(DriverItem::Event(Err(error))) => Err(error),
            Some(DriverItem::Event(Ok(_))) | None => {
                Err(Error::Api("stream driver ended before opening"))
            }
        }
    }
}

impl Stream for ChatStream<'_> {
    type Item = Result<ChatStreamEvent, Error>;

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
pub(crate) struct ProviderStream<'a> {
    events: EventStream<ByteStream<'a>>,
    /// `None` after provider completion or a terminal parser error.
    parser: Option<ProviderSse>,
    /// Provider completion was observed; drain the remaining HTTP framing so
    /// the Agent HTTP client can safely reuse the connection.
    drain_after_done: bool,
    queue: VecDeque<Result<ChatStreamEvent, Error>>,
}

impl<'a> ProviderStream<'a> {
    pub(crate) fn new(
        bytes: impl Stream<Item = Result<ResponsePart, Error>> + 'a,
        parser: ProviderSse,
    ) -> Self {
        let bytes = bytes.map(|part| match part? {
            ResponsePart::Data(chunk) => Ok(chunk),
            ResponsePart::Head(_) => Err(Error::Api("HTTP response head was emitted twice")),
        });
        Self {
            events: EventStream::new(Box::pin(bytes)),
            parser: Some(parser),
            drain_after_done: false,
            queue: VecDeque::new(),
        }
    }
}

impl Stream for ProviderStream<'_> {
    type Item = Result<ChatStreamEvent, Error>;

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
                Poll::Ready(Some(Err(EventStreamError::Utf8(_) | EventStreamError::Parser(_)))) => {
                    this.parser = None;
                    return Poll::Ready(Some(Err(Error::Parse)));
                }
                Poll::Ready(None) => {
                    if this.drain_after_done {
                        this.drain_after_done = false;
                        return Poll::Ready(None);
                    }
                    this.parser = None;
                    return Poll::Ready(Some(Err(Error::truncated_stream())));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Drain a byte stream to a UTF-8 string. Used to read a non-2xx error body
/// before failing a streaming request.
pub(crate) async fn drain_body<S>(mut stream: S) -> Result<String, Error>
where
    S: Stream<Item = Result<ResponsePart, Error>> + Unpin,
{
    let mut buf = Vec::new();
    while let Some(part) = stream.next().await {
        match part? {
            ResponsePart::Head(_) => return Err(Error::HttpCodec),
            ResponsePart::Data(chunk) => buf.extend_from_slice(&chunk),
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

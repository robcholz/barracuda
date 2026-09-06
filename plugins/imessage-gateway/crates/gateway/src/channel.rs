use alloc::boxed::Box;
use core::{future::Future, pin::Pin};

use crate::{
    ChannelError, DeleteMessageRequest, EditMessageRequest, MediaKind, Operation, ReactRequest,
    SendMediaRequest, SendMessageRequest, SendReceipt, SendStreamRequest, SetTypingRequest,
    StreamError, TextChunk,
};
use futures_lite::{stream, StreamExt};
use serde::Deserialize;

/// Local, executor-neutral future returned by a message-channel provider.
pub type ChannelFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ChannelError>> + 'a>>;

/// One concrete outbound messaging channel such as Telegram, WeChat, or iMessage.
pub trait MessageChannel: 'static {
    /// Stable name used by [`crate::MessageTarget::channel`].
    fn channel(&self) -> &str;

    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt>;

    /// Sends one ordered stream of semantic Agent events.
    ///
    /// The default projection consumes every event and forwards only
    /// `output_delta.payload.text` to [`MessageChannel::send_message`]. Rich
    /// channels override this method to consume additional event types.
    fn send_stream(&self, request: SendStreamRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            let SendStreamRequest {
                target,
                events,
                reply_to,
            } = request;
            let chunks = stream::unfold(events, |mut events| async move {
                loop {
                    match events.next().await {
                        Some(Ok(event)) if event.event_type == "output_delta" => {
                            let payload = match serde_json::from_str::<OutputDelta<'_>>(
                                event.payload.as_str(),
                            ) {
                                Ok(payload) => payload,
                                Err(_error) => {
                                    return Some((
                                        Err(StreamError::failed("invalid output_delta payload")),
                                        events,
                                    ));
                                }
                            };
                            let Some(text) = TextChunk::inline(payload.text) else {
                                return Some((
                                    Err(StreamError::failed("output_delta exceeds stream frame")),
                                    events,
                                ));
                            };
                            return Some((Ok(text), events));
                        }
                        Some(Ok(_event)) => {}
                        Some(Err(error)) => return Some((Err(error), events)),
                        None => return None,
                    }
                }
            });
            let mut request = SendMessageRequest::stream(target, Box::pin(chunks));
            request.reply_to = reply_to;
            self.send_message(request).await
        })
    }

    fn send_media(
        &self,
        kind: MediaKind,
        _request: SendMediaRequest,
    ) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move { Err(ChannelError::unsupported(kind.operation())) })
    }

    fn edit_message(&self, _request: EditMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::EditMessage)) })
    }

    fn delete_message(&self, _request: DeleteMessageRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::DeleteMessage)) })
    }

    fn react(&self, _request: ReactRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::React)) })
    }

    fn set_typing(&self, _request: SetTypingRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::SetTyping)) })
    }
}

#[derive(Deserialize)]
struct OutputDelta<'a> {
    #[serde(borrow)]
    text: &'a str,
}

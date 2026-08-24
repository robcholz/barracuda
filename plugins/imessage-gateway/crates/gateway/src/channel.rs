use alloc::boxed::Box;
use core::{future::Future, pin::Pin};

use crate::{
    ChannelError, DeleteMessageRequest, EditMessageRequest, MediaKind, Operation, ReactRequest,
    SendMediaRequest, SendMessageRequest, SendReceipt, SendStreamField, SendStreamRequest,
    SetTypingRequest,
};
use futures_lite::{stream, StreamExt};

/// Local, executor-neutral future returned by a message-channel provider.
pub type ChannelFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ChannelError>> + 'a>>;

/// One concrete outbound messaging channel such as Telegram, WeChat, or iMessage.
pub trait MessageChannel: 'static {
    /// Stable name used by [`crate::MessageTarget::channel`].
    fn channel(&self) -> &str;

    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt>;

    /// Sends one ordered primary-text stream with optional extra-content frames.
    ///
    /// The default projection consumes every frame and forwards only primary
    /// text to [`MessageChannel::send_message`]. Rich channels override this
    /// method to interpret extra fields.
    fn send_stream(&self, request: SendStreamRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            let SendStreamRequest {
                target,
                frames,
                reply_to,
            } = request;
            let chunks = stream::unfold(frames, |mut frames| async move {
                loop {
                    match frames.next().await {
                        Some(Ok(frame)) if frame.field == SendStreamField::Text => {
                            return Some((Ok(frame.text), frames));
                        }
                        Some(Ok(_extra)) => {}
                        Some(Err(error)) => return Some((Err(error), frames)),
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

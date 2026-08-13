use alloc::boxed::Box;
use core::{future::Future, pin::Pin};

use crate::{
    ChannelError, DeleteMessageRequest, EditMessageRequest, MediaKind, Operation, ReactRequest,
    SendMediaRequest, SendMessageRequest, SendReceipt, SetTypingRequest,
};

/// Local, executor-neutral future returned by a message-channel provider.
pub type ChannelFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ChannelError>> + 'a>>;

/// One concrete outbound messaging channel such as Telegram, WeChat, or iMessage.
pub trait MessageChannel: 'static {
    /// Stable name used by [`crate::MessageTarget::channel`].
    fn channel(&self) -> &str;

    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt>;

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

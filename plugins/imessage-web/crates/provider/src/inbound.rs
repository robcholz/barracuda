use alloc::{boxed::Box, string::String, vec::Vec};
use core::{future::Future, pin::Pin};

use barracuda_imessage_gateway_plugin::Attachment;

use crate::WebControl;

/// Runtime-neutral future returned by an inbound Web message sink.
pub type InboundFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, InboundError>> + 'a>>;

/// A message received from a Web client: text, files the client uploaded
/// first, or both.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboundMessage {
    pub conversation_id: String,
    pub message_id: String,
    pub thread_id: Option<String>,
    /// Empty for a message of attachments only.
    pub text: String,
    /// Files already stored by the Gateway, in the order the client named them.
    pub attachments: Vec<Attachment>,
}

/// Control request received from a Web client for its conversation's turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboundControl {
    pub conversation_id: String,
    pub thread_id: Option<String>,
    pub control: WebControl,
    pub temporary: bool,
    pub session: Option<String>,
    pub title: Option<String>,
    pub confirm: bool,
}

/// Consumer of messages received from Web clients.
pub trait InboundMessageSink: 'static {
    fn receive_message(&self, request: InboundMessage) -> InboundFuture<'_, ()>;

    fn receive_control(&self, _request: InboundControl) -> InboundFuture<'_, ()> {
        Box::pin(async {
            Err(InboundError::Unsupported {
                operation: "receive_control",
            })
        })
    }
}

/// Sink failure while receiving a Web message.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InboundError {
    #[error("operation is not supported: {operation}")]
    Unsupported { operation: &'static str },
    #[error("inbound sink rejected the request: {message}")]
    Rejected { message: String },
}

use alloc::{boxed::Box, string::String, vec::Vec};
use core::pin::Pin;

use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::StreamError;

/// Presentation role of an outbound text message.
///
/// Lets clients that support secondary or system content render it distinctly
/// from the primary reply. Providers that do not distinguish roles render every
/// kind as ordinary text. This is an IM-level presentation hint, not agent
/// internals: senders map their own content onto these generic roles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// The primary, user-visible reply.
    #[default]
    Reply,
    /// Secondary reasoning or "thinking" text.
    Reasoning,
    /// A tool-execution notice.
    Tool,
    /// Any other secondary or system notice (for example usage metadata).
    Notice,
}

/// Asynchronous append-only chunks for one text message.
pub type TextStream = Pin<Box<dyn Stream<Item = Result<String, StreamError>> + 'static>>;

/// Asynchronous chunks for one binary payload.
pub type BinaryStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, StreamError>> + 'static>>;

/// Destination selected by its registered channel name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageTarget {
    pub channel: String,
    pub conversation_id: String,
    pub thread_id: Option<String>,
}

impl MessageTarget {
    pub fn new(channel: impl Into<String>, conversation_id: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            conversation_id: conversation_id.into(),
            thread_id: None,
        }
    }
}

/// Text supplied either in full or as append-only asynchronous chunks.
pub enum TextBody {
    Complete(String),
    Stream(TextStream),
}

/// Binary data supplied in memory or as asynchronous chunks.
pub enum BinaryBody {
    Bytes(Vec<u8>),
    Stream(BinaryStream),
}

/// A text-message send operation.
pub struct SendMessageRequest {
    pub target: MessageTarget,
    pub body: TextBody,
    pub reply_to: Option<String>,
    pub kind: MessageKind,
}

impl SendMessageRequest {
    pub fn text(target: MessageTarget, text: impl Into<String>) -> Self {
        Self {
            target,
            body: TextBody::Complete(text.into()),
            reply_to: None,
            kind: MessageKind::Reply,
        }
    }

    pub fn stream(target: MessageTarget, stream: TextStream) -> Self {
        Self {
            target,
            body: TextBody::Stream(stream),
            reply_to: None,
            kind: MessageKind::Reply,
        }
    }

    /// Sets the presentation role for this message.
    #[must_use]
    pub fn with_kind(mut self, kind: MessageKind) -> Self {
        self.kind = kind;
        self
    }
}

/// A file, image, audio, or video send operation.
pub struct SendMediaRequest {
    pub target: MessageTarget,
    pub body: BinaryBody,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub caption: Option<String>,
    pub reply_to: Option<String>,
}

impl SendMediaRequest {
    pub fn bytes(
        target: MessageTarget,
        filename: impl Into<String>,
        mime_type: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            target,
            body: BinaryBody::Bytes(bytes),
            filename: Some(filename.into()),
            mime_type: Some(mime_type.into()),
            caption: None,
            reply_to: None,
        }
    }

    pub fn stream(
        target: MessageTarget,
        filename: impl Into<String>,
        mime_type: impl Into<String>,
        stream: BinaryStream,
    ) -> Self {
        Self {
            target,
            body: BinaryBody::Stream(stream),
            filename: Some(filename.into()),
            mime_type: Some(mime_type.into()),
            caption: None,
            reply_to: None,
        }
    }
}

/// Update the text of an existing message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditMessageRequest {
    pub target: MessageTarget,
    pub message_id: String,
    pub text: String,
}

impl EditMessageRequest {
    pub fn new(
        target: MessageTarget,
        message_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            target,
            message_id: message_id.into(),
            text: text.into(),
        }
    }
}

/// Delete an existing message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteMessageRequest {
    pub target: MessageTarget,
    pub message_id: String,
}

impl DeleteMessageRequest {
    pub fn new(target: MessageTarget, message_id: impl Into<String>) -> Self {
        Self {
            target,
            message_id: message_id.into(),
        }
    }
}

/// Add or replace a reaction on an existing message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReactRequest {
    pub target: MessageTarget,
    pub message_id: String,
    pub reaction: String,
}

impl ReactRequest {
    pub fn new(
        target: MessageTarget,
        message_id: impl Into<String>,
        reaction: impl Into<String>,
    ) -> Self {
        Self {
            target,
            message_id: message_id.into(),
            reaction: reaction.into(),
        }
    }
}

/// Set the typing state for a conversation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetTypingRequest {
    pub target: MessageTarget,
    pub typing: bool,
}

impl SetTypingRequest {
    pub fn new(target: MessageTarget, typing: bool) -> Self {
        Self { target, typing }
    }
}

/// Platform identifier returned after a successful send or edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendReceipt {
    pub message_id: String,
}

impl SendReceipt {
    pub fn new(message_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
        }
    }
}

/// Kind of binary message selected by the Gateway facade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaKind {
    File,
    Image,
    Audio,
    Video,
}

impl MediaKind {
    pub const fn operation(self) -> Operation {
        match self {
            Self::File => Operation::SendFile,
            Self::Image => Operation::SendImage,
            Self::Audio => Operation::SendAudio,
            Self::Video => Operation::SendVideo,
        }
    }
}

/// Public operations used in typed unsupported errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    SendMessage,
    SendFile,
    SendImage,
    SendAudio,
    SendVideo,
    EditMessage,
    DeleteMessage,
    React,
    SetTyping,
}

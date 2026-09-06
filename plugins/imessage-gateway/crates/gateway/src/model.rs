use alloc::{boxed::Box, string::String, vec::Vec};
use core::{fmt, ops::Deref, pin::Pin};

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

const INLINE_TEXT_BYTES: usize = 512;
const INLINE_BINARY_BYTES: usize = 384;

/// UTF-8 stream chunk stored inline unless a provider already owns a String.
#[derive(Clone)]
pub struct TextChunk {
    storage: TextChunkStorage,
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)] // Deliberately avoids one heap allocation per stream chunk.
enum TextChunkStorage {
    Owned(String),
    Inline {
        bytes: [u8; INLINE_TEXT_BYTES],
        len: u16,
    },
}

impl TextChunk {
    /// Copies one lane-bounded UTF-8 chunk into inline storage.
    #[must_use]
    pub fn inline(text: &str) -> Option<Self> {
        let len = u16::try_from(text.len()).ok()?;
        let length = usize::from(len);
        if length > INLINE_TEXT_BYTES {
            return None;
        }
        let mut bytes = [0_u8; INLINE_TEXT_BYTES];
        bytes.get_mut(..length)?.copy_from_slice(text.as_bytes());
        Some(Self {
            storage: TextChunkStorage::Inline { bytes, len },
        })
    }

    /// Borrows the UTF-8 chunk.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match &self.storage {
            TextChunkStorage::Owned(text) => text,
            TextChunkStorage::Inline { bytes, len } => {
                core::str::from_utf8(bytes.get(..usize::from(*len)).unwrap_or_default())
                    .unwrap_or_default()
            }
        }
    }

    /// Converts the chunk into provider-owned text when retention is required.
    #[must_use]
    pub fn into_string(self) -> String {
        match self.storage {
            TextChunkStorage::Owned(text) => text,
            TextChunkStorage::Inline { bytes, len } => String::from(
                core::str::from_utf8(bytes.get(..usize::from(len)).unwrap_or_default())
                    .unwrap_or_default(),
            ),
        }
    }

    /// Returns whether the chunk occupies only its inline storage.
    #[must_use]
    pub const fn is_inline(&self) -> bool {
        matches!(&self.storage, TextChunkStorage::Inline { .. })
    }
}

impl From<String> for TextChunk {
    fn from(text: String) -> Self {
        Self {
            storage: TextChunkStorage::Owned(text),
        }
    }
}

impl From<&str> for TextChunk {
    fn from(text: &str) -> Self {
        Self::from(String::from(text))
    }
}

impl Deref for TextChunk {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl PartialEq for TextChunk {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for TextChunk {}

impl fmt::Debug for TextChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(formatter)
    }
}

/// Binary stream chunk stored inline unless a provider already owns a Vec.
#[derive(Clone)]
pub struct BinaryChunk {
    storage: BinaryChunkStorage,
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)] // Deliberately avoids one heap allocation per stream chunk.
enum BinaryChunkStorage {
    Owned(Vec<u8>),
    Inline {
        bytes: [u8; INLINE_BINARY_BYTES],
        len: u16,
    },
}

impl BinaryChunk {
    /// Creates empty inline storage for one JSON lane's decoded Base64 payload.
    #[must_use]
    pub const fn empty_inline() -> Self {
        Self {
            storage: BinaryChunkStorage::Inline {
                bytes: [0; INLINE_BINARY_BYTES],
                len: 0,
            },
        }
    }

    /// Appends one decoded byte, returning false when inline storage is full.
    pub fn push(&mut self, byte: u8) -> bool {
        let BinaryChunkStorage::Inline { bytes, len } = &mut self.storage else {
            return false;
        };
        let Some(destination) = bytes.get_mut(usize::from(*len)) else {
            return false;
        };
        *destination = byte;
        let Some(next) = len.checked_add(1) else {
            return false;
        };
        *len = next;
        true
    }

    /// Borrows the binary chunk.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match &self.storage {
            BinaryChunkStorage::Owned(bytes) => bytes,
            BinaryChunkStorage::Inline { bytes, len } => {
                bytes.get(..usize::from(*len)).unwrap_or_default()
            }
        }
    }

    /// Converts the chunk into provider-owned bytes when retention is required.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        match self.storage {
            BinaryChunkStorage::Owned(bytes) => bytes,
            BinaryChunkStorage::Inline { bytes, len } => bytes
                .get(..usize::from(len))
                .map(Vec::from)
                .unwrap_or_default(),
        }
    }

    /// Returns whether the chunk occupies only its inline storage.
    #[must_use]
    pub const fn is_inline(&self) -> bool {
        matches!(&self.storage, BinaryChunkStorage::Inline { .. })
    }
}

impl From<Vec<u8>> for BinaryChunk {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            storage: BinaryChunkStorage::Owned(bytes),
        }
    }
}

impl Deref for BinaryChunk {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl PartialEq for BinaryChunk {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for BinaryChunk {}

impl fmt::Debug for BinaryChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(formatter)
    }
}

/// Asynchronous append-only chunks for one text message.
pub type TextStream = Pin<Box<dyn Stream<Item = Result<TextChunk, StreamError>> + 'static>>;

/// Ordered content field carried by one full Gateway send stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendStreamField {
    /// Primary text understood by every channel.
    Text,
    /// Optional model reasoning metadata.
    Reasoning,
    /// Optional effect-produced result metadata.
    EffectResult,
    /// Optional user-facing notice metadata.
    Notice,
    /// Generic producer lifecycle or metadata event.
    Event,
    /// Start of one structured tool result.
    ToolResultStart,
    /// Provider tool-call identifier.
    ToolCallId,
    /// Tool name.
    ToolName,
    /// Tool arguments JSON.
    ToolArguments,
    /// Tool output text.
    ToolOutput,
    /// Successful tool completion marker.
    ToolSucceeded,
    /// Failed tool completion marker.
    ToolFailed,
    /// End of one structured tool result.
    ToolResultEnd,
}

/// Whether more chunks belong to the current stream field.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamBoundary {
    /// More chunks follow for this field.
    More,
    /// This chunk completes the field.
    Complete,
}

/// One ordered primary-text or extra-content frame delivered to a channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendStreamFrame {
    /// Semantic content carried by this frame.
    pub field: SendStreamField,
    /// Current field's chunk boundary.
    pub boundary: StreamBoundary,
    /// UTF-8 content; marker fields carry an empty string.
    pub text: TextChunk,
}

impl SendStreamFrame {
    /// Creates one channel-facing stream frame.
    #[must_use]
    pub fn new(field: SendStreamField, boundary: StreamBoundary, text: impl Into<String>) -> Self {
        Self {
            field,
            boundary,
            text: TextChunk::from(text.into()),
        }
    }

    /// Creates a frame whose text remains inline with the stream item.
    #[must_use]
    pub fn inline(field: SendStreamField, boundary: StreamBoundary, text: &str) -> Option<Self> {
        Some(Self {
            field,
            boundary,
            text: TextChunk::inline(text)?,
        })
    }
}

/// Ordered frame stream for one outbound Gateway delivery.
pub type SendStream = Pin<Box<dyn Stream<Item = Result<SendStreamFrame, StreamError>> + 'static>>;

/// Asynchronous chunks for one binary payload.
pub type BinaryStream = Pin<Box<dyn Stream<Item = Result<BinaryChunk, StreamError>> + 'static>>;

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

/// One full outbound Gateway stream with primary text and optional extra frames.
pub struct SendStreamRequest {
    /// Destination provider and conversation.
    pub target: MessageTarget,
    /// Ordered primary-text and extra-content frames.
    pub frames: SendStream,
    /// Optional provider message being replied to.
    pub reply_to: Option<String>,
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

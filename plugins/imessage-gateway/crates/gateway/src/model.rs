use alloc::{boxed::Box, string::String, vec::Vec};

use barracuda_bulk_memory::BulkText;
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

/// Largest text chunk one stream frame carries.
const FRAME_TEXT_BYTES: usize = 512;
const INLINE_BINARY_BYTES: usize = 384;

/// UTF-8 stream chunk held in bulk memory and shared on clone.
///
/// Queued and retained chunks therefore cost a pointer in internal RAM rather
/// than a fixed inline buffer.
#[derive(Clone, PartialEq, Eq)]
pub struct TextChunk(BulkText);

impl TextChunk {
    /// Copies one frame-bounded UTF-8 chunk, or `None` when `text` exceeds a
    /// stream frame.
    #[must_use]
    pub fn inline(text: &str) -> Option<Self> {
        (text.len() <= FRAME_TEXT_BYTES).then(|| Self(BulkText::new(text)))
    }

    /// Borrows the UTF-8 chunk.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Copies the chunk into provider-owned text.
    #[must_use]
    pub fn into_string(self) -> String {
        String::from(self.as_str())
    }
}

impl From<String> for TextChunk {
    fn from(text: String) -> Self {
        Self::from(text.as_str())
    }
}

impl From<&str> for TextChunk {
    fn from(text: &str) -> Self {
        Self(BulkText::new(text))
    }
}

impl Deref for TextChunk {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl PartialEq<str> for TextChunk {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for TextChunk {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

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

/// One complete JSON value carried by a semantic event, held in bulk memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonContent(TextChunk);

impl JsonContent {
    /// Borrows the encoded JSON value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Copies the value into provider-owned JSON text.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0.into_string()
    }
}

/// Takes already encoded JSON text without copying it.
impl From<BulkText> for JsonContent {
    fn from(json: BulkText) -> Self {
        Self(TextChunk(json))
    }
}

impl From<String> for JsonContent {
    fn from(json: String) -> Self {
        Self(TextChunk::from(json))
    }
}

impl From<&str> for JsonContent {
    fn from(json: &str) -> Self {
        Self(TextChunk::from(json))
    }
}

/// One complete semantic event delivered in an outbound Gateway stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendStreamEvent {
    /// Agent session that owns the event stream.
    pub session: String,
    /// Agent-assigned event order.
    pub sequence: u64,
    /// Forward-compatible semantic event type.
    pub event_type: String,
    /// Complete JSON object payload encoded without an outer envelope.
    pub payload: JsonContent,
}

impl SendStreamEvent {
    /// Creates one provider-facing semantic event.
    #[must_use]
    pub fn new(
        session: impl Into<String>,
        sequence: u64,
        event_type: impl Into<String>,
        payload: impl Into<JsonContent>,
    ) -> Self {
        Self {
            session: session.into(),
            sequence,
            event_type: event_type.into(),
            payload: payload.into(),
        }
    }
}

/// Ordered semantic event stream for one outbound Gateway delivery.
pub type SendStream = Pin<Box<dyn Stream<Item = Result<SendStreamEvent, StreamError>> + 'static>>;

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

/// One full outbound Gateway stream of semantic Agent events.
pub struct SendStreamRequest {
    /// Destination provider and conversation.
    pub target: MessageTarget,
    /// Ordered semantic events, including turn lifecycle records.
    pub events: SendStream,
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

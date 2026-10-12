use alloc::{rc::Rc, string::String};

use barracuda_imessage_gateway_plugin::{MediaKind, MessageKind, SendStreamEvent, TextChunk};

/// One sequenced event emitted to Web clients.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebEvent {
    pub id: u64,
    pub conversation_id: String,
    pub thread_id: Option<String>,
    pub data: WebEventData,
}

/// Message operation represented by a Web event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebEventData {
    MessageStart {
        message_id: String,
        reply_to: Option<String>,
        kind: MessageKind,
    },
    MessageDelta {
        message_id: String,
        /// Shares the Gateway chunk's bulk allocation.
        delta: TextChunk,
    },
    /// One complete semantic Agent event carried by the message stream.
    MessageEvent {
        message_id: String,
        /// Shares the Gateway event's bulk payload.
        event: SendStreamEvent,
    },
    MessageEnd {
        message_id: String,
        error: Option<String>,
    },
    /// One whole file, image, audio or video message. The bytes stay on the
    /// device; a page fetches them from `url`.
    Media {
        message_id: String,
        kind: MediaKind,
        /// `/api/web/media/<id>`, served while the file stays kept.
        url: String,
        mime_type: String,
        filename: Option<String>,
        size: u64,
        caption: Option<String>,
        reply_to: Option<String>,
    },
    MessageEdit {
        message_id: String,
        text: String,
    },
    MessageDelete {
        message_id: String,
    },
    MessageReaction {
        message_id: String,
        reaction: String,
    },
    ConversationTyping {
        typing: bool,
    },
    /// The conversation's sessions after a session command, as the compact
    /// JSON of a `SendSessionsRequest`. Every open page of the conversation
    /// gets it, so a switch made in one tab reaches the others.
    ConversationSessions {
        json: String,
    },
}

/// A normal event, an explicit notification that replay history was lost, or
/// the refusal of one client's message frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebDelivery {
    /// Shared with the replay history and every other subscriber.
    Event(Rc<WebEvent>),
    Lagged {
        missed: u64,
    },
    /// A message frame the bridge refused, told only to the client that sent
    /// it: nothing was published and none of its attachments was consumed.
    Rejected {
        /// The id the bridge numbered the frame with (`web-in-<n>`).
        message_id: String,
        error: Rejection,
    },
}

/// Why the bridge refused a message frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rejection {
    /// The frame carried neither text nor an attachment.
    Empty,
    /// The frame named more than [`crate::MAX_ATTACHMENTS`] attachments.
    TooManyAttachments,
    /// An attachment id was never uploaded, was already sent, or was evicted.
    UnknownAttachment,
}

impl Rejection {
    /// The stable code a page reads.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooManyAttachments => "too_many_attachments",
            Self::UnknownAttachment => "unknown_attachment",
        }
    }
}

impl WebEventData {
    pub(crate) const fn event_name(&self) -> &'static str {
        match self {
            Self::MessageStart { .. } => "message.start",
            Self::MessageDelta { .. } => "message.delta",
            Self::MessageEvent { .. } => "message.event",
            Self::MessageEnd { .. } => "message.end",
            Self::Media { kind, .. } => match kind {
                MediaKind::File => "message.file",
                MediaKind::Image => "message.image",
                MediaKind::Audio => "message.audio",
                MediaKind::Video => "message.video",
            },
            Self::MessageEdit { .. } => "message.edit",
            Self::MessageDelete { .. } => "message.delete",
            Self::MessageReaction { .. } => "message.reaction",
            Self::ConversationTyping { .. } => "conversation.typing",
            Self::ConversationSessions { .. } => "conversation.sessions",
        }
    }
}

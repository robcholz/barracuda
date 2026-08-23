use alloc::{string::String, vec::Vec};

use gateway::{MediaKind, MessageKind};

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
        delta: String,
    },
    MessageEnd {
        message_id: String,
        error: Option<String>,
    },
    Media {
        message_id: String,
        kind: MediaKind,
        phase: MediaPhase,
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
}

/// Lifecycle of a streamed binary message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaPhase {
    Start {
        filename: Option<String>,
        mime_type: Option<String>,
        caption: Option<String>,
        reply_to: Option<String>,
    },
    Delta {
        bytes: Vec<u8>,
    },
    End {
        error: Option<String>,
    },
}

/// A normal event or an explicit notification that replay history was lost.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebDelivery {
    Event(WebEvent),
    Lagged { missed: u64 },
}

impl WebEventData {
    pub(crate) const fn event_name(&self) -> &'static str {
        match self {
            Self::MessageStart { .. } => "message.start",
            Self::MessageDelta { .. } => "message.delta",
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
        }
    }
}

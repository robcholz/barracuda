//! Messages delivered to agents.

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

pub use barracuda_agent_message::{Attachment, MediaKind};

/// One message delivered to an agent: text and the files sent with it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<Attachment>,
}

impl Message {
    /// Build a text message.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    /// This message with `attachments` sent along.
    #[must_use]
    pub fn with_attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.attachments = attachments;
        self
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The files sent with the message, by reference.
    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    /// Whether the message carries nothing: no visible text and no files.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn photo() -> Attachment {
        Attachment {
            kind: MediaKind::Image,
            path: "/workspace/cache/gateway/web/1.jpg".into(),
            mime_type: "image/jpeg".into(),
            filename: None,
            size: 3,
        }
    }

    #[test]
    fn text_message_projects_to_plain_text() {
        let message = Message::text("hello");

        assert_eq!(message.as_str(), "hello");
        assert!(message.attachments().is_empty());
    }

    #[test]
    fn wire_shape_carries_text_and_attachments() {
        let encoded = serde_json::to_value(Message::text("hello")).expect("message encodes");
        assert_eq!(encoded, serde_json::json!({ "text": "hello" }));

        let wire = serde_json::json!({
            "text": "",
            "attachments": [{
                "kind": "image",
                "path": "/workspace/cache/gateway/web/1.jpg",
                "mime_type": "image/jpeg",
                "size": 3
            }]
        });
        let decoded: Message = serde_json::from_value(wire.clone()).expect("message decodes");
        assert_eq!(decoded, Message::text("").with_attachments(vec![photo()]));
        assert_eq!(
            serde_json::to_value(&decoded).expect("message encodes"),
            wire
        );
        assert!(serde_json::from_value::<Message>(serde_json::json!({
            "text": "hello",
            "files": []
        }))
        .is_err());
    }

    #[test]
    fn a_message_of_only_attachments_is_not_empty() {
        assert!(Message::text(" \n").is_empty());
        assert!(!Message::text(" ")
            .with_attachments(vec![photo()])
            .is_empty());
    }
}

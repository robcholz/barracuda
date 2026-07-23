//! Messages delivered to sessions and agents.

use serde::{Deserialize, Serialize};

/// One message delivered to a session or agent.
///
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    text: String,
}

impl Message {
    /// Build a text message.
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_message_projects_to_plain_text() {
        let message = Message::text("hello");

        assert_eq!(message.as_str(), "hello");
    }

    #[test]
    fn wire_shape_contains_only_current_text_input() {
        let encoded = serde_json::to_value(Message::text("hello")).expect("message encodes");

        assert_eq!(encoded, serde_json::json!({ "text": "hello" }));
        assert!(serde_json::from_value::<Message>(serde_json::json!({
            "text": "hello",
            "attachments": []
        }))
        .is_err());
    }
}

use alloc::string::String;

use serde::{Deserialize, Serialize};

/// Gateway-owned destination and provider conversation identity.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct GatewayRoute {
    /// Registered message-channel name.
    pub channel: String,
    /// Provider conversation identifier.
    pub conversation_id: String,
    /// Optional provider thread identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

impl GatewayRoute {
    /// Creates a route without a thread selection.
    pub fn new(channel: impl Into<String>, conversation_id: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            conversation_id: conversation_id.into(),
            thread_id: None,
        }
    }

    /// Selects a provider thread within this conversation.
    #[must_use]
    pub fn with_thread(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::GatewayRoute;

    #[test]
    fn omits_an_absent_thread_from_the_wire_route() {
        let route = GatewayRoute::new("web", "conversation");

        assert_eq!(
            serde_json::to_value(route).ok(),
            Some(json!({
                "channel": "web",
                "conversation_id": "conversation"
            }))
        );
    }

    #[test]
    fn serializes_a_present_thread_as_a_string() {
        let route = GatewayRoute::new("telegram", "conversation").with_thread("topic");

        assert_eq!(
            serde_json::to_value(route).ok(),
            Some(json!({
                "channel": "telegram",
                "conversation_id": "conversation",
                "thread_id": "topic"
            }))
        );
    }
}

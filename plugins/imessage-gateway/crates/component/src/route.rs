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

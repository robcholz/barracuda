use alloc::string::String;

use serde::{Deserialize, Serialize};

/// A frame sent from a Web client to the server.
///
/// The client supplies only content; the server assigns each message a stable
/// id within the connection.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WebClientFrame {
    /// Message text.
    pub text: String,
    /// Optional provider message id being replied to.
    #[serde(default)]
    pub reply_to: Option<String>,
}

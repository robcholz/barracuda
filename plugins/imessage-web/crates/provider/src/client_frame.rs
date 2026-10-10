use alloc::string::String;

use serde::{Deserialize, Serialize};

/// A frame sent from a Web client to the server.
///
/// The client supplies only content; the server assigns each message a stable
/// id within the connection. Replies are the client's concern: a client that
/// quotes an earlier message puts the quote in `text`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WebClientFrame {
    /// Message text.
    pub text: String,
}

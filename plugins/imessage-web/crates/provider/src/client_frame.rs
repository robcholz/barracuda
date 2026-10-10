use alloc::string::String;

use serde::{Deserialize, Serialize};

/// What a Web client asks of the turn running in its conversation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebControl {
    /// Stop at the turn's next iteration boundary, keeping what it produced.
    Interrupt,
    /// Abort the turn at once (the client withdraws the message it answers).
    Cancel,
}

/// A control frame sent from a Web client: `{ "control": "interrupt" }` or
/// `{ "control": "cancel" }`. It names no message: it acts on whatever turn
/// the conversation's session is running, and takes no message id.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebClientControl {
    /// The requested control.
    pub control: WebControl,
}

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

/// A client frame: a control for the running turn, or a message.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[cfg_attr(not(any(test, feature = "server")), allow(dead_code))]
pub(crate) enum ClientFrame {
    Control(WebClientControl),
    Message(WebClientFrame),
}

#[cfg(test)]
mod tests {
    use super::{ClientFrame, WebControl};

    #[test]
    fn a_frame_is_a_control_or_a_message() {
        assert!(matches!(
            serde_json::from_str(r#"{"control":"interrupt"}"#),
            Ok(ClientFrame::Control(frame)) if frame.control == WebControl::Interrupt
        ));
        assert!(matches!(
            serde_json::from_str(r#"{"control":"cancel"}"#),
            Ok(ClientFrame::Control(frame)) if frame.control == WebControl::Cancel
        ));
        assert!(matches!(
            serde_json::from_str(r#"{"text":"hi"}"#),
            Ok(ClientFrame::Message(frame)) if frame.text == "hi"
        ));
        // an older client's reply id is ignored, not refused
        assert!(matches!(
            serde_json::from_str(r#"{"text":"hi","reply_to":"web-1"}"#),
            Ok(ClientFrame::Message(frame)) if frame.text == "hi"
        ));
        assert!(serde_json::from_str::<ClientFrame>(r#"{"control":"stop"}"#).is_err());
    }
}

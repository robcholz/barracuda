//! WebSocket wire glue shared by the terminal client and the host server.
//!
//! Client → server frames are [`barracuda_imessage_web_plugin::WebClientFrame`].
//! Server → client frames are the web channel's own Server-Sent-Events serialization
//! (`WebDelivery::to_sse`); the client parses them with [`parse_sse`].

/// One parsed Server-Sent-Events frame received from the server.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SseFrame {
    /// The `event:` name, e.g. `message.start`.
    pub event: String,
    /// The `data:` payload, still JSON-encoded.
    pub data: String,
}

/// Parses one complete SSE frame (`id:`/`event:`/`data:` lines).
///
/// Returns `None` when the frame has neither an event name nor data.
#[must_use]
pub fn parse_sse(frame: &str) -> Option<SseFrame> {
    let mut parsed = SseFrame::default();
    let mut saw_field = false;
    for line in frame.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            parsed.event = value.trim().to_string();
            saw_field = true;
        } else if let Some(value) = line.strip_prefix("data:") {
            parsed.data = value.trim().to_string();
            saw_field = true;
        }
    }
    if saw_field {
        Some(parsed)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_event_and_data_lines() {
        let frame = "id: 7\nevent: message.delta\ndata: {\"delta\":\"hi\"}\n\n";
        let parsed = parse_sse(frame).expect("parse");
        assert_eq!(parsed.event, "message.delta");
        assert_eq!(parsed.data, "{\"delta\":\"hi\"}");
    }

    #[test]
    fn ignores_frames_without_fields() {
        assert_eq!(parse_sse("\n\n"), None);
    }
}

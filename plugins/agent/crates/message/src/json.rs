//! JSON encoding of chat messages.
//!
//! The general writer lives in [`barracuda_json_writer`]; this module adds the
//! message array and re-exports the rest so callers keep one import path.

pub use barracuda_json_writer::*;

use crate::ChatMessage;

/// Encodes `messages` as one JSON array.
pub fn write_messages<'a>(
    sink: &mut dyn Sink,
    messages: impl IntoIterator<Item = &'a ChatMessage>,
) {
    sink.put(b"[");
    for (index, message) in messages.into_iter().enumerate() {
        if index > 0 {
            sink.put(b",");
        }
        sink.put(message.as_bytes());
    }
    sink.put(b"]");
}

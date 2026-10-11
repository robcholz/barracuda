//! References to files sent with a message.
//!
//! A message never carries file bytes: it names a file by path. A user or
//! tool message with attachments stores them in a transcript as `media`
//! parts of its `content` array:
//!
//! ```json
//! {"content":[{"text":"look","type":"text"},
//!             {"filename":"cat.jpg","kind":"image","mime_type":"image/jpeg",
//!              "path":"/workspace/cache/gateway/web/1.jpg","size":48213,"type":"media"}],
//!  "role":"user"}
//! ```
//!
//! A `media` part is internal: request encoders replace it with the provider's
//! image or document block, or with the [`Attachment::placeholder`] text.

use alloc::format;
use alloc::string::String;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json::{Object, Sink};

/// The `type` of a content part that references an attachment.
pub const MEDIA_PART_TYPE: &str = "media";

/// How a channel presents a file.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// Generic file.
    File,
    /// Image.
    Image,
    /// Audio.
    Audio,
    /// Video.
    Video,
}

impl MediaKind {
    /// The wire name of this kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }
}

/// Reference to one file in the Plugins' shared filesystem view.
///
/// Only this reference travels through Workflow, Events, transcripts, and
/// Tool results; the bytes stay in the file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    /// How a channel presents the file.
    pub kind: MediaKind,
    /// Path of the file, such as `/workspace/cache/gateway/telegram/…`.
    pub path: String,
    /// MIME type of the content.
    pub mime_type: String,
    /// Name the sender gave the file, when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Length of the file in bytes.
    pub size: u64,
}

impl Attachment {
    /// The name shown for the file: its filename, or else its path's last
    /// segment.
    #[must_use]
    pub fn name(&self) -> &str {
        match self.filename.as_deref() {
            Some(name) if !name.is_empty() => name,
            _ => self.path.rsplit('/').next().unwrap_or(&self.path),
        }
    }

    /// The text a request carries instead of a file it does not inline.
    ///
    /// `available` is false when the file no longer exists, for example after
    /// a restart cleared the cache it was stored in.
    #[must_use]
    pub fn placeholder(&self, available: bool) -> String {
        let suffix = if available {
            ""
        } else {
            ", no longer available"
        };
        format!(
            "[attachment: {}, {}, {} bytes, {}{suffix}]",
            self.name(),
            self.mime_type,
            self.size,
            self.path
        )
    }

    /// A short label for plain-text renderings, such as `[image: cat.jpg]`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("[{}: {}]", self.kind.as_str(), self.name())
    }

    /// Writes this attachment as one `media` content part, in key order.
    pub fn write_part(&self, sink: &mut dyn Sink) {
        let mut part = Object::begin(sink);
        if let Some(filename) = &self.filename {
            part.str("filename", filename);
        }
        part.str("kind", self.kind.as_str());
        part.str("mime_type", &self.mime_type);
        part.str("path", &self.path);
        part.value("size", &Value::from(self.size));
        part.str("type", MEDIA_PART_TYPE);
        part.end();
    }

    /// Reads a `media` content part; any other part is `None`.
    #[must_use]
    pub fn from_part(part: &Value) -> Option<Self> {
        let object = part.as_object()?;
        if object.get("type").and_then(Value::as_str) != Some(MEDIA_PART_TYPE) {
            return None;
        }
        let text = |name: &str| object.get(name).and_then(Value::as_str);
        Some(Self {
            kind: serde_json::from_value(object.get("kind")?.clone()).ok()?,
            path: text("path")?.into(),
            mime_type: text("mime_type")?.into(),
            filename: text("filename").map(Into::into),
            size: object.get("size").and_then(Value::as_u64)?,
        })
    }
}

/// The MIME type of a JPEG, PNG, GIF or WebP image or a PDF, recognized by
/// its first bytes, the only kinds a model reads directly.
#[must_use]
pub fn sniff_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

/// How many leading bytes [`sniff_media_type`] reads.
pub const SNIFF_BYTES: usize = 12;

/// Whether an encoded message has a `media` content part.
///
/// Encoded messages are compact `serde_json`, and quotes inside strings are
/// escaped, so this only matches a part's own `type` field.
#[must_use]
pub fn has_media(encoded: &str) -> bool {
    encoded.contains(r#""type":"media""#)
}

/// The plain text of a message `content` field.
///
/// A string is returned as is. In a content array, text parts keep their text
/// and `media` parts become their [`Attachment::label`], one per line. Other
/// values have no text.
#[must_use]
pub fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                let piece = match Attachment::from_part(part) {
                    Some(attachment) => attachment.label(),
                    None => match part.get("text").and_then(Value::as_str) {
                        Some(piece) => piece.into(),
                        None => continue,
                    },
                };
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&piece);
            }
            Some(text)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    fn photo() -> Attachment {
        Attachment {
            kind: MediaKind::Image,
            path: "/workspace/cache/gateway/web/1.jpg".into(),
            mime_type: "image/jpeg".into(),
            filename: Some("cat.jpg".into()),
            size: 48_213,
        }
    }

    #[test]
    fn a_part_is_written_in_key_order_and_read_back() {
        let part = crate::json::encode_string(|sink| photo().write_part(sink));
        let value: Value = serde_json::from_str(&part).unwrap();
        assert_eq!(part, serde_json::to_string(&value).unwrap());
        assert_eq!(
            value,
            json!({"filename": "cat.jpg", "kind": "image", "mime_type": "image/jpeg",
                   "path": "/workspace/cache/gateway/web/1.jpg", "size": 48213, "type": "media"})
        );
        assert_eq!(Attachment::from_part(&value), Some(photo()));
        assert_eq!(
            Attachment::from_part(&json!({"type": "text", "text": "x"})),
            None
        );
    }

    #[test]
    fn placeholders_and_labels_name_the_file() {
        let mut nameless = photo();
        nameless.filename = None;
        assert_eq!(nameless.name(), "1.jpg");
        assert_eq!(
            photo().placeholder(true),
            "[attachment: cat.jpg, image/jpeg, 48213 bytes, /workspace/cache/gateway/web/1.jpg]"
        );
        assert_eq!(
            photo().placeholder(false),
            "[attachment: cat.jpg, image/jpeg, 48213 bytes, /workspace/cache/gateway/web/1.jpg, no longer available]"
        );
        let part: Value =
            serde_json::from_str(&crate::json::encode_string(|sink| photo().write_part(sink)))
                .unwrap();
        assert_eq!(
            content_text(&json!([{"type": "text", "text": "look"}, part])),
            Some("look\n[image: cat.jpg]".into())
        );
        assert_eq!(content_text(&json!("plain")), Some("plain".into()));
    }

    #[test]
    fn only_a_media_part_is_detected() {
        assert!(has_media(
            r#"{"content":[{"size":1,"type":"media"}],"role":"user"}"#
        ));
        assert!(!has_media(
            r#"{"content":"say \"type\":\"media\"","role":"user"}"#
        ));
    }
}

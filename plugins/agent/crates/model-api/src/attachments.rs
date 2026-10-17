//! Attachments a request inlines.
//!
//! User and tool messages reference files by `media` content parts
//! ([`barracuda_agent_message::attachment`]); those parts never reach a
//! provider as they are. Once per call, before the first attempt, [`resolve`]
//! walks the request from its newest message back and reads each file it may
//! inline from the request's filesystem into bulk memory:
//!
//! - an image (declared `image` kind or `image/*` type) when the `image`
//!   modality is on and its bytes are JPEG, PNG, GIF or WebP;
//! - a PDF (`application/pdf` or a `.pdf` name) when the `pdf` modality is on,
//!   the backend takes documents, and its bytes start with `%PDF-`;
//!
//! up to `max_images_in_context` files in all, each at most
//! `image_max_bytes`. The body encoders then base64 those bytes straight into
//! the request body. Every other part becomes a text placeholder naming the
//! file ([`Attachment::placeholder`]), marked as no longer available when the
//! file is gone, so the model can still reach it with file tools.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_agent_message::attachment::{has_media, sniff_media_type, Attachment, MediaKind};
use barracuda_agent_message::json::{write_object, write_value, Object, Sink};
use barracuda_agent_message::ChatMessage;
use barracuda_bulk_memory::BulkVec;
use barracuda_vfs::{FsError, ScopedVfs};
use embedded_io_async::Read as _;
use serde_json::{json, Map, Value};

use super::types::{ChatRequest, InputModalities};

/// The `type` of the object that stands for one inlined part while a backend
/// converts messages; [`write_with_media`] replaces it with the provider block.
const MARKER_TYPE: &str = "media_ref";

/// What a backend may inline, from its configuration.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MediaPolicy {
    pub(crate) modalities: InputModalities,
    /// Whether the backend takes PDF document blocks.
    pub(crate) documents: bool,
    pub(crate) max_inline: usize,
    pub(crate) max_bytes: usize,
}

/// One inlined file.
pub(crate) enum Inline {
    Image {
        media_type: &'static str,
        bytes: BulkVec<u8>,
    },
    Pdf(BulkVec<u8>),
}

enum Resolved {
    Inline(Inline),
    /// The file no longer exists.
    Missing,
}

/// The files one request inlines, by (message, content part) position, where
/// messages count `messages` followed by `reminders`.
#[derive(Default)]
pub(crate) struct RequestMedia {
    parts: BTreeMap<(usize, usize), Resolved>,
}

impl RequestMedia {
    /// The inlined file at a part, if the request inlines it.
    pub(crate) fn inline(&self, message: usize, part: usize) -> Option<&Inline> {
        match self.parts.get(&(message, part)) {
            Some(Resolved::Inline(inline)) => Some(inline),
            _ => None,
        }
    }

    /// The text for a part the request does not inline.
    pub(crate) fn placeholder(
        &self,
        message: usize,
        part: usize,
        attachment: &Attachment,
    ) -> String {
        let missing = matches!(self.parts.get(&(message, part)), Some(Resolved::Missing));
        attachment.placeholder(!missing)
    }

    /// The replacement for one `media` part: a marker for an inlined file
    /// `accepts` takes, otherwise a text part with its placeholder.
    pub(crate) fn part(
        &self,
        message: usize,
        part: usize,
        attachment: &Attachment,
        accepts: fn(&Inline) -> bool,
    ) -> Value {
        match self.inline(message, part) {
            Some(inline) if accepts(inline) => marker(message, part),
            _ => json!({"text": self.placeholder(message, part, attachment), "type": "text"}),
        }
    }
}

/// Reads the files `request` may inline under `policy`.
pub(crate) async fn resolve(policy: MediaPolicy, request: &ChatRequest<'_>) -> RequestMedia {
    let mut media = RequestMedia::default();
    let Some(files) = request.files else {
        return media;
    };
    let messages: Vec<&ChatMessage> = request.messages.iter().chain(request.reminders).collect();
    let mut inlined = 0_usize;
    for (index, message) in messages.iter().enumerate().rev() {
        if !has_media(message.as_str()) {
            continue;
        }
        let value = message.to_value();
        let Some(parts) = value.get("content").and_then(Value::as_array) else {
            continue;
        };
        for (part, content) in parts.iter().enumerate().rev() {
            let Some(attachment) = Attachment::from_part(content) else {
                continue;
            };
            let resolved = resolve_one(policy, files, &attachment, inlined).await;
            if matches!(resolved, Some(Resolved::Inline(_))) {
                inlined = inlined.saturating_add(1);
            }
            if let Some(resolved) = resolved {
                media.parts.insert((index, part), resolved);
            }
        }
    }
    media
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Candidate {
    Image,
    Pdf,
}

fn candidate(attachment: &Attachment) -> Option<Candidate> {
    let mime = attachment
        .mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    if attachment.kind == MediaKind::Image
        || mime
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
    {
        return Some(Candidate::Image);
    }
    let name = attachment.name();
    let pdf_name = name
        .len()
        .checked_sub(4)
        .and_then(|start| name.get(start..))
        .is_some_and(|extension| extension.eq_ignore_ascii_case(".pdf"));
    (mime.eq_ignore_ascii_case("application/pdf") || pdf_name).then_some(Candidate::Pdf)
}

async fn resolve_one(
    policy: MediaPolicy,
    files: &ScopedVfs,
    attachment: &Attachment,
    inlined: usize,
) -> Option<Resolved> {
    let eligible = match candidate(attachment) {
        Some(Candidate::Image) => policy.modalities.image.then_some(Candidate::Image),
        Some(Candidate::Pdf) => {
            (policy.modalities.pdf && policy.documents).then_some(Candidate::Pdf)
        }
        None => None,
    }
    .filter(|_| inlined < policy.max_inline);
    let Some(eligible) = eligible else {
        // A file that is not inlined only needs to say whether it still exists.
        return match files.exists(&attachment.path).await {
            Ok(false) => Some(Resolved::Missing),
            Ok(true) | Err(_) => None,
        };
    };
    let bytes = match read_capped(files, &attachment.path, policy.max_bytes).await {
        Ok(bytes) => bytes,
        Err(ReadError::Missing) => return Some(Resolved::Missing),
        Err(ReadError::TooLarge | ReadError::Unreadable) => return None,
    };
    match eligible {
        Candidate::Image => image_media_type(&bytes)
            .map(|media_type| Resolved::Inline(Inline::Image { media_type, bytes })),
        Candidate::Pdf => (sniff_media_type(&bytes) == Some("application/pdf"))
            .then(|| Resolved::Inline(Inline::Pdf(bytes))),
    }
}

enum ReadError {
    Missing,
    TooLarge,
    Unreadable,
}

/// Reads a whole file of at most `max_bytes` into bulk memory.
async fn read_capped(
    files: &ScopedVfs,
    path: &str,
    max_bytes: usize,
) -> Result<BulkVec<u8>, ReadError> {
    let not_found = |error: FsError| match error {
        FsError::NotFound => ReadError::Missing,
        _ => ReadError::Unreadable,
    };
    let len = files.len(path).await.map_err(not_found)?;
    let len = usize::try_from(len)
        .ok()
        .filter(|len| *len <= max_bytes)
        .ok_or(ReadError::TooLarge)?;
    let mut file = files.open(path).await.map_err(not_found)?;
    let mut bytes = BulkVec::try_with_capacity(len).map_err(|_error| ReadError::TooLarge)?;
    bytes.resize(len, 0);
    file.read_exact(&mut bytes)
        .await
        .map_err(|_error| ReadError::Unreadable)?;
    Ok(bytes)
}

/// The MIME type of a JPEG, PNG, GIF or WebP image, from its first bytes.
fn image_media_type(bytes: &[u8]) -> Option<&'static str> {
    sniff_media_type(bytes).filter(|media_type| media_type.starts_with("image/"))
}

fn marker(message: usize, part: usize) -> Value {
    json!({"message": message, "part": part, "type": MARKER_TYPE})
}

fn marker_position(object: &Map<String, Value>) -> Option<(usize, usize)> {
    if object.get("type").and_then(Value::as_str) != Some(MARKER_TYPE) {
        return None;
    }
    let index = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
    };
    Some((index("message")?, index("part")?))
}

/// Replaces the `media` parts of a decoded message's `content` array, each
/// with [`RequestMedia::part`].
pub(crate) fn replace_parts(
    message: &mut Value,
    index: usize,
    media: &RequestMedia,
    accepts: fn(&Inline) -> bool,
) {
    let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for (part, content) in parts.iter_mut().enumerate() {
        if let Some(attachment) = Attachment::from_part(content) {
            *content = media.part(index, part, &attachment, accepts);
        }
    }
}

/// Writes `value` as `serde_json` would, with each marker written by `render`
/// from the inlined file it stands for.
pub(crate) fn write_with_media(
    sink: &mut dyn Sink,
    value: &Value,
    media: &RequestMedia,
    render: fn(&mut dyn Sink, &Inline),
) {
    match value {
        Value::Array(items) => {
            sink.put(b"[");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    sink.put(b",");
                }
                write_with_media(sink, item, media, render);
            }
            sink.put(b"]");
        }
        Value::Object(object) => write_object_with_media(sink, object, media, render),
        other => write_value(sink, other),
    }
}

/// [`write_with_media`] for an object.
pub(crate) fn write_object_with_media(
    sink: &mut dyn Sink,
    object: &Map<String, Value>,
    media: &RequestMedia,
    render: fn(&mut dyn Sink, &Inline),
) {
    if let Some((message, part)) = marker_position(object) {
        if let Some(inline) = media.inline(message, part) {
            render(sink, inline);
            return;
        }
    }
    if !object
        .values()
        .any(|value| value.is_array() || value.is_object())
    {
        write_object(sink, object);
        return;
    }
    let mut writer = Object::begin(sink);
    for (name, value) in object {
        write_with_media(writer.field(name), value, media, render);
    }
    writer.end();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn images_are_recognized_by_their_first_bytes() {
        assert_eq!(
            image_media_type(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(
            image_media_type(b"\x89PNG\r\n\x1a\n...."),
            Some("image/png")
        );
        assert_eq!(image_media_type(b"GIF89a.."), Some("image/gif"));
        assert_eq!(
            image_media_type(b"RIFF\x10\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        // HEIC is an ISO media file: `ftypheic` after the box length.
        assert_eq!(image_media_type(b"\0\0\0\x18ftypheic"), None);
        assert_eq!(image_media_type(b""), None);
    }

    #[test]
    fn declared_kind_type_or_name_makes_a_candidate() {
        let attachment = |kind, mime_type: &str, filename: Option<&str>| Attachment {
            kind,
            path: "/workspace/cache/x".into(),
            mime_type: mime_type.into(),
            filename: filename.map(Into::into),
            size: 1,
        };
        assert!(
            candidate(&attachment(
                MediaKind::Image,
                "application/octet-stream",
                None
            )) == Some(Candidate::Image)
        );
        assert!(
            candidate(&attachment(MediaKind::File, "IMAGE/PNG", None)) == Some(Candidate::Image)
        );
        assert!(
            candidate(&attachment(MediaKind::File, "application/pdf", None))
                == Some(Candidate::Pdf)
        );
        assert!(
            candidate(&attachment(
                MediaKind::File,
                "application/octet-stream",
                Some("a.PDF")
            )) == Some(Candidate::Pdf)
        );
        assert!(candidate(&attachment(MediaKind::Audio, "audio/ogg", None)).is_none());
    }
}

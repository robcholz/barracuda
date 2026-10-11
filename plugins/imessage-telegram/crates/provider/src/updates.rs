//! Receiving through the Bot API's `getUpdates` long poll.
//!
//! Only the request paths and the response decoding live here; the channel
//! Plugin owns the connection, the cursor, and what happens to each message.
//! Responses decode straight into the few fields a message needs, and every
//! other field is skipped without building a JSON tree.

use alloc::{format, string::String, vec::Vec};

use barracuda_imessage_gateway_plugin::MediaKind;
use serde::Deserialize;

/// Seconds the server holds a `getUpdates` request open while it has no
/// update; the Bot API's maximum.
pub const POLL_TIMEOUT_SECS: u32 = 50;

/// Most updates one `getUpdates` response carries.
pub const UPDATE_BATCH: u8 = 8;

/// `allowed_updates=["message"]`, URL-encoded: only new messages, never
/// edits, channel posts, or callbacks.
const ALLOWED_UPDATES: &str = "%5B%22message%22%5D";

/// Path of one `getUpdates` long poll, relative to the API base.
///
/// `offset` (the last handled `update_id` plus one) acknowledges every
/// earlier update; without it the server starts from its oldest unconfirmed
/// update. The path carries the bot token: never log it.
#[must_use]
pub fn get_updates_path(token: &str, offset: Option<i64>, limit: u8) -> String {
    let mut path = format!(
        "/bot{token}/getUpdates?timeout={POLL_TIMEOUT_SECS}&limit={limit}&allowed_updates={ALLOWED_UPDATES}"
    );
    if let Some(offset) = offset {
        path.push_str(&format!("&offset={offset}"));
    }
    path
}

/// Path of `deleteWebhook`, which `getUpdates` needs while a webhook is set.
/// Pending updates are kept.
#[must_use]
pub fn delete_webhook_path(token: &str) -> String {
    format!("/bot{token}/deleteWebhook")
}

/// Path of `getFile`, which names where the file of `file_id` can be
/// downloaded. The path carries the bot token: never log it.
#[must_use]
pub fn get_file_path(token: &str, file_id: &str) -> String {
    let mut path = format!("/bot{token}/getFile?file_id=");
    for byte in file_id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            path.push(char::from(byte));
        } else {
            path.push_str(&format!("%{byte:02X}"));
        }
    }
    path
}

/// Path of a file's content, from the `file_path` that `getFile` answered.
/// The path carries the bot token: never log it.
#[must_use]
pub fn file_download_path(token: &str, file_path: &str) -> String {
    format!("/file/bot{token}/{}", file_path.trim_start_matches('/'))
}

/// One update of a `getUpdates` response.
#[derive(Debug, Deserialize)]
pub struct Update {
    /// Sequential identifier; the next poll's offset is the last one plus one.
    pub update_id: i64,
    /// The new message, when the update is one.
    #[serde(default)]
    pub message: Option<Message>,
}

/// The fields of an incoming message the channel uses.
#[derive(Debug, Deserialize)]
pub struct Message {
    /// Identifier within its chat; a reply's `reply_to`.
    pub message_id: i64,
    /// Sender; absent for messages sent on behalf of a chat.
    #[serde(default)]
    pub from: Option<User>,
    /// The chat the message belongs to; its `id` is where replies go.
    pub chat: Chat,
    /// Text of a text message; absent for media, stickers, and service messages.
    #[serde(default)]
    pub text: Option<String>,
    /// Caption of a media message.
    #[serde(default)]
    pub caption: Option<String>,
    /// A photo in every size the server keeps, smallest first.
    #[serde(default)]
    pub photo: Vec<PhotoSize>,
    /// A general file. Telegram also sets it beside `animation`.
    #[serde(default)]
    pub document: Option<FileRef>,
    /// A voice note.
    #[serde(default)]
    pub voice: Option<FileRef>,
    /// An audio file meant for a music player.
    #[serde(default)]
    pub audio: Option<FileRef>,
    /// A video.
    #[serde(default)]
    pub video: Option<FileRef>,
    /// A round video message.
    #[serde(default)]
    pub video_note: Option<FileRef>,
    /// A GIF or a silent H.264 video.
    #[serde(default)]
    pub animation: Option<FileRef>,
}

impl Message {
    /// The message's text: its `text`, else its media's `caption`.
    #[must_use]
    pub fn body(&self) -> Option<&str> {
        self.text.as_deref().or(self.caption.as_deref())
    }

    /// The file the message carries, if any; a sticker carries none here.
    ///
    /// Of a photo's sizes it picks the largest whose `file_size` is at most
    /// `max_bytes`, else the largest of all, which the caller's size check
    /// then refuses.
    #[must_use]
    pub fn media(&self, max_bytes: u64) -> Option<Media<'_>> {
        if let Some(photo) = largest_photo(&self.photo, max_bytes) {
            return Some(Media {
                kind: MediaKind::Image,
                file_id: &photo.file_id,
                file_size: photo.file_size,
                // Telegram stores every photo as a JPEG.
                mime_type: Some("image/jpeg"),
                file_name: None,
            });
        }
        let (kind, file, default_mime) = if let Some(file) = &self.voice {
            (MediaKind::Audio, file, Some("audio/ogg"))
        } else if let Some(file) = &self.audio {
            (MediaKind::Audio, file, None)
        } else if let Some(file) = &self.video {
            (MediaKind::Video, file, None)
        } else if let Some(file) = &self.video_note {
            // Round videos are MP4 and carry no MIME type.
            (MediaKind::Video, file, Some("video/mp4"))
        } else if let Some(file) = &self.animation {
            // Checked before `document`, which Telegram sets beside it.
            (MediaKind::Video, file, None)
        } else if let Some(file) = &self.document {
            let kind = if file.mime_type.as_deref().is_some_and(is_model_image) {
                MediaKind::Image
            } else {
                MediaKind::File
            };
            (kind, file, None)
        } else {
            return None;
        };
        Some(Media {
            kind,
            file_id: &file.file_id,
            file_size: file.file_size,
            mime_type: file
                .mime_type
                .as_deref()
                .filter(|mime| !mime.is_empty())
                .or(default_mime),
            file_name: file.file_name.as_deref().filter(|name| !name.is_empty()),
        })
    }
}

/// Image types models accept, so a document of one is an image.
fn is_model_image(mime_type: &str) -> bool {
    let essence = mime_type.split(';').next().unwrap_or_default().trim();
    ["image/jpeg", "image/png", "image/gif", "image/webp"]
        .iter()
        .any(|image| image.eq_ignore_ascii_case(essence))
}

/// The largest size within `max_bytes`, else the largest of all.
fn largest_photo(sizes: &[PhotoSize], max_bytes: u64) -> Option<&PhotoSize> {
    sizes
        .iter()
        .filter(|size| size.file_size.is_some_and(|bytes| bytes <= max_bytes))
        .max_by_key(|size| size.file_size)
        .or_else(|| sizes.last())
}

/// One size of a photo.
#[derive(Debug, Deserialize)]
pub struct PhotoSize {
    /// Identifier for `getFile`.
    pub file_id: String,
    /// Size in bytes, when known.
    #[serde(default)]
    pub file_size: Option<u64>,
}

/// A document, voice note, audio, video, round video, or animation.
#[derive(Debug, Deserialize)]
pub struct FileRef {
    /// Identifier for `getFile`.
    pub file_id: String,
    /// Size in bytes, when known.
    #[serde(default)]
    pub file_size: Option<u64>,
    /// MIME type the sender's client reported.
    #[serde(default)]
    pub mime_type: Option<String>,
    /// Name the sender gave the file.
    #[serde(default)]
    pub file_name: Option<String>,
}

/// The file of one message, as [`Message::media`] picks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Media<'a> {
    /// How the channel presents the file.
    pub kind: MediaKind,
    /// Identifier for `getFile`.
    pub file_id: &'a str,
    /// Size in bytes, when known.
    pub file_size: Option<u64>,
    /// MIME type, when Telegram or the sender's client names one.
    pub mime_type: Option<&'a str>,
    /// Name the sender gave the file.
    pub file_name: Option<&'a str>,
}

/// The `getFile` answer: where to download a file.
#[derive(Debug, Deserialize)]
pub struct File {
    /// Size in bytes, when known.
    #[serde(default)]
    pub file_size: Option<u64>,
    /// Path for [`file_download_path`]; absent when the file cannot be
    /// downloaded.
    #[serde(default)]
    pub file_path: Option<String>,
}

/// A message's sender.
#[derive(Debug, Deserialize)]
pub struct User {
    /// Stable user identifier.
    pub id: i64,
    /// First name, always present.
    #[serde(default)]
    pub first_name: String,
    /// Public `@` name, when the user set one.
    #[serde(default)]
    pub username: Option<String>,
}

impl User {
    /// Label shown for an owner: `@username`, or the first name without one.
    #[must_use]
    pub fn label(&self) -> String {
        match self.username.as_deref() {
            Some(username) if !username.is_empty() => format!("@{username}"),
            _ => self.first_name.clone(),
        }
    }
}

/// A message's chat.
#[derive(Debug, Deserialize)]
pub struct Chat {
    /// Chat identifier: the user's own id in a private chat, negative for
    /// groups.
    pub id: i64,
}

/// The Bot API refused a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP status.
    pub status: u16,
    /// The Bot API's `error_code`, which matches the status in practice.
    pub error_code: Option<i64>,
    /// The Bot API's `description`, shown to the user.
    pub description: String,
    /// Seconds to wait before retrying, sent with 429.
    pub retry_after: Option<u32>,
}

impl ApiError {
    /// The error code, falling back to the HTTP status.
    #[must_use]
    pub fn code(&self) -> i64 {
        self.error_code.unwrap_or(i64::from(self.status))
    }

    /// 409 because a webhook is set; `deleteWebhook` fixes it.
    #[must_use]
    pub fn is_webhook_conflict(&self) -> bool {
        self.code() == 409 && self.description.contains("webhook")
    }

    /// 401 or 404: the token is wrong or revoked, which no retry fixes.
    #[must_use]
    pub fn is_bad_token(&self) -> bool {
        matches!(self.code(), 401 | 404)
    }
}

/// Why a Bot API response gave no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdatesError {
    /// The Bot API answered `"ok": false`, or a non-2xx status.
    Api(ApiError),
    /// The body was not a Bot API response.
    Malformed {
        /// HTTP status.
        status: u16,
    },
}

#[derive(Deserialize)]
struct Envelope<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    error_code: Option<i64>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<Parameters>,
}

#[derive(Deserialize)]
struct Parameters {
    #[serde(default)]
    retry_after: Option<u32>,
}

/// Decodes a `getUpdates` response.
///
/// # Errors
///
/// Returns [`UpdatesError::Api`] when the Bot API refused the poll and
/// [`UpdatesError::Malformed`] when the body is not a Bot API response.
pub fn parse_updates(status: u16, body: &[u8]) -> Result<Vec<Update>, UpdatesError> {
    parse(status, body).map(Option::unwrap_or_default)
}

/// Decodes a `getFile` response.
///
/// # Errors
///
/// As [`parse_updates`]; a successful answer without a result is
/// [`UpdatesError::Malformed`].
pub fn parse_file(status: u16, body: &[u8]) -> Result<File, UpdatesError> {
    parse(status, body)?.ok_or(UpdatesError::Malformed { status })
}

/// Decodes a response whose result the caller does not need, such as
/// `deleteWebhook`.
///
/// # Errors
///
/// As [`parse_updates`].
pub fn parse_acknowledgement(status: u16, body: &[u8]) -> Result<(), UpdatesError> {
    parse::<serde::de::IgnoredAny>(status, body).map(|_result| ())
}

fn parse<T: for<'de> Deserialize<'de>>(
    status: u16,
    body: &[u8],
) -> Result<Option<T>, UpdatesError> {
    let envelope: Envelope<T> = match serde_json::from_slice(body) {
        Ok(envelope) => envelope,
        // A refusal from a proxy or a load balancer carries no Bot API body.
        Err(_error) if !(200..300).contains(&status) => {
            return Err(UpdatesError::Api(ApiError {
                status,
                error_code: None,
                description: format!("HTTP {status}"),
                retry_after: None,
            }))
        }
        Err(_error) => return Err(UpdatesError::Malformed { status }),
    };
    if envelope.ok && (200..300).contains(&status) {
        return Ok(envelope.result);
    }
    Err(UpdatesError::Api(ApiError {
        status,
        error_code: envelope.error_code,
        description: envelope
            .description
            .unwrap_or_else(|| format!("HTTP {status}")),
        retry_after: envelope
            .parameters
            .and_then(|parameters| parameters.retry_after),
    }))
}

/// The first `update_id` in the prefix of a response too large to decode, so
/// the update it names can be acknowledged and skipped.
///
/// The Bot API writes `update_id` as each update's first field.
#[must_use]
pub fn first_update_id(prefix: &[u8]) -> Option<i64> {
    const KEY: &[u8] = b"\"update_id\"";
    let start = prefix
        .windows(KEY.len())
        .position(|window| window == KEY)?
        .checked_add(KEY.len())?;
    let rest = prefix.get(start..)?;
    let rest = rest
        .iter()
        .position(|byte| !matches!(byte, b' ' | b':' | b'\t' | b'\r' | b'\n'))
        .and_then(|skip| rest.get(skip..))?;
    let digits = rest
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(rest.len());
    core::str::from_utf8(rest.get(..digits)?).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    use super::*;

    #[test]
    fn poll_paths_carry_the_cursor_and_filter() {
        assert_eq!(
            get_updates_path("T", None, 8),
            "/botT/getUpdates?timeout=50&limit=8&allowed_updates=%5B%22message%22%5D"
        );
        assert!(get_updates_path("T", Some(43), 1)
            .ends_with("&limit=1&allowed_updates=%5B%22message%22%5D&offset=43"));
        assert_eq!(delete_webhook_path("T"), "/botT/deleteWebhook");
    }

    #[test]
    fn decodes_text_and_skips_everything_else() {
        let body = br#"{"ok":true,"result":[
            {"update_id":7,"message":{"message_id":3,"date":1,"from":{"id":42,"is_bot":false,"first_name":"Ann","username":"ann"},"chat":{"id":42,"type":"private"},"text":"hi","entities":[]}},
            {"update_id":8,"message":{"message_id":4,"from":{"id":43,"first_name":"Bo"},"chat":{"id":-5},"photo":[{"file_id":"x"}]}},
            {"update_id":9,"edited_message":{"message_id":1}}
        ]}"#;
        let updates = parse_updates(200, body).unwrap();
        assert_eq!(updates.len(), 3);
        let first = updates[0].message.as_ref().unwrap();
        assert_eq!(first.text.as_deref(), Some("hi"));
        assert_eq!(first.from.as_ref().unwrap().label(), "@ann");
        let second = updates[1].message.as_ref().unwrap();
        assert!(second.text.is_none());
        assert_eq!(second.chat.id, -5);
        assert_eq!(second.from.as_ref().unwrap().label(), "Bo");
        assert!(updates[2].message.is_none());
    }

    #[test]
    fn file_paths_carry_the_token_and_encode_the_id() {
        assert_eq!(
            get_file_path("T", "AgAC-x_y"),
            "/botT/getFile?file_id=AgAC-x_y"
        );
        assert_eq!(
            get_file_path("T", "a+b/c"),
            "/botT/getFile?file_id=a%2Bb%2Fc"
        );
        assert_eq!(
            file_download_path("T", "photos/file_1.jpg"),
            "/file/botT/photos/file_1.jpg"
        );
    }

    fn message(fields: &str) -> Message {
        let body = format!(
            r#"{{"ok":true,"result":[{{"update_id":1,"message":{{"message_id":1,"chat":{{"id":1}}{fields}}}}}]}}"#
        );
        parse_updates(200, body.as_bytes())
            .unwrap()
            .pop()
            .unwrap()
            .message
            .unwrap()
    }

    #[test]
    fn a_photo_is_its_largest_size_within_the_limit() {
        let photo = message(
            r#","caption":"look","photo":[{"file_id":"s","file_size":10,"width":90},{"file_id":"m","file_size":500},{"file_id":"l","file_size":5000}]"#,
        );
        assert_eq!(photo.body(), Some("look"));
        let media = photo.media(1000).unwrap();
        assert_eq!(media.kind, MediaKind::Image);
        assert_eq!(media.file_id, "m");
        assert_eq!(media.mime_type, Some("image/jpeg"));
        assert_eq!(photo.media(10_000).unwrap().file_id, "l");
        // None fits: the largest, which the size check refuses.
        assert_eq!(photo.media(5).unwrap().file_id, "l");
    }

    #[test]
    fn media_kinds_follow_the_field_and_the_mime_type() {
        let kind = |fields: &str| {
            let message = message(fields);
            message
                .media(u64::MAX)
                .map(|media| (media.kind, media.mime_type.map(String::from)))
        };
        assert_eq!(
            kind(r#","voice":{"file_id":"v","mime_type":"audio/ogg","file_size":3}"#),
            Some((MediaKind::Audio, Some("audio/ogg".into())))
        );
        assert_eq!(
            kind(r#","voice":{"file_id":"v"}"#),
            Some((MediaKind::Audio, Some("audio/ogg".into())))
        );
        assert_eq!(
            kind(r#","audio":{"file_id":"a","mime_type":"audio/mpeg"}"#),
            Some((MediaKind::Audio, Some("audio/mpeg".into())))
        );
        assert_eq!(
            kind(r#","video":{"file_id":"v","mime_type":"video/mp4"}"#),
            Some((MediaKind::Video, Some("video/mp4".into())))
        );
        assert_eq!(
            kind(r#","video_note":{"file_id":"n","length":240}"#),
            Some((MediaKind::Video, Some("video/mp4".into())))
        );
        assert_eq!(
            kind(
                r#","animation":{"file_id":"g","mime_type":"video/mp4"},"document":{"file_id":"g","mime_type":"video/mp4"}"#
            ),
            Some((MediaKind::Video, Some("video/mp4".into())))
        );
        assert_eq!(
            kind(r#","document":{"file_id":"d","mime_type":"image/png","file_name":"a.png"}"#),
            Some((MediaKind::Image, Some("image/png".into())))
        );
        assert_eq!(
            kind(r#","document":{"file_id":"d","mime_type":"image/heic"}"#),
            Some((MediaKind::File, Some("image/heic".into())))
        );
        assert_eq!(
            kind(r#","document":{"file_id":"d"}"#),
            Some((MediaKind::File, None))
        );
        assert_eq!(kind(r#","sticker":{"file_id":"s","emoji":"👍"}"#), None);
        let named = message(r#","document":{"file_id":"d","file_name":"report.pdf"}"#);
        assert_eq!(named.media(u64::MAX).unwrap().file_name, Some("report.pdf"));
    }

    #[test]
    fn get_file_answers_the_download_path() {
        let file = parse_file(
            200,
            br#"{"ok":true,"result":{"file_id":"x","file_unique_id":"u","file_size":12,"file_path":"photos/file_1.jpg"}}"#,
        )
        .unwrap();
        assert_eq!(file.file_path.as_deref(), Some("photos/file_1.jpg"));
        assert_eq!(file.file_size, Some(12));
        let UpdatesError::Api(error) = parse_file(
            400,
            br#"{"ok":false,"error_code":400,"description":"Bad Request: file is too big"}"#,
        )
        .unwrap_err() else {
            panic!("not an API error");
        };
        assert_eq!(error.description, "Bad Request: file is too big");
        assert_eq!(
            parse_file(200, br#"{"ok":true}"#).unwrap_err(),
            UpdatesError::Malformed { status: 200 }
        );
    }

    #[test]
    fn refusals_keep_the_description_and_retry_after() {
        let error = parse_updates(
            429,
            br#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 7","parameters":{"retry_after":7}}"#,
        )
        .unwrap_err();
        let UpdatesError::Api(error) = error else {
            panic!("not an API error");
        };
        assert_eq!(error.retry_after, Some(7));
        assert!(!error.is_webhook_conflict());

        let UpdatesError::Api(webhook) = parse_updates(
            409,
            br#"{"ok":false,"error_code":409,"description":"Conflict: can't use getUpdates method while webhook is active; use deleteWebhook to delete the webhook first"}"#,
        )
        .unwrap_err() else {
            panic!("not an API error");
        };
        assert!(webhook.is_webhook_conflict());

        let UpdatesError::Api(other) = parse_updates(
            409,
            br#"{"ok":false,"error_code":409,"description":"Conflict: terminated by other getUpdates request; make sure that only one bot instance is running"}"#,
        )
        .unwrap_err() else {
            panic!("not an API error");
        };
        assert!(!other.is_webhook_conflict());

        let UpdatesError::Api(token) = parse_updates(
            404,
            br#"{"ok":false,"error_code":404,"description":"Not Found"}"#,
        )
        .unwrap_err() else {
            panic!("not an API error");
        };
        assert!(token.is_bad_token());

        assert_eq!(
            parse_updates(502, b"<html>bad gateway</html>").unwrap_err(),
            UpdatesError::Api(ApiError {
                status: 502,
                error_code: None,
                description: "HTTP 502".into(),
                retry_after: None,
            })
        );
        assert_eq!(
            parse_updates(200, b"nonsense").unwrap_err(),
            UpdatesError::Malformed { status: 200 }
        );
        assert_eq!(
            parse_acknowledgement(200, br#"{"ok":true,"result":true}"#),
            Ok(())
        );
    }

    #[test]
    fn finds_the_first_update_id_in_a_prefix() {
        assert_eq!(
            first_update_id(
                br#"{"ok":true,"result":[{"update_id": 123456,"message":{"text":"aaaa"#
            ),
            Some(123_456)
        );
        assert_eq!(first_update_id(br#"{"ok":true,"result":[{"upd"#), None);
    }
}

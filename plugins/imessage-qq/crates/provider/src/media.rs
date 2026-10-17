//! Sending files: the rich-media upload and the message that carries it.
//!
//! The QQ Bot API takes a file for a direct or group chat in two requests
//! (<https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/send-receive/rich-media.html>,
//! `POST /v2/users/{openid}/files` and `/v2/groups/{group_openid}/files`):
//! the upload, `{"file_type","srv_send_msg":false,"file_name","file_data"}`
//! with the bytes base64-encoded in `file_data` (a device has no public URL
//! for the `url` form), answers `file_info`; a message with `msg_type` 7 and
//! `media: {file_info}` then sends it. Tencent's own Node SDK
//! (`@tencent-connect/qqbot-nodejs`, `MediaApi`) builds the same bodies.

use alloc::{boxed::Box, format, rc::Rc, string::String, vec::Vec};

use barracuda_imessage_gateway_plugin::{
    mime_extension, BinaryBody, BinaryChunk, BinaryStream, ChannelError, MediaKind, MediaSource,
    StreamError,
};
use base64::Engine as _;
use futures_lite::{stream, StreamExt as _};

/// Largest file `file_data` takes: QQ documents 5 MB for a binary upload,
/// and the URL and chunked forms beyond that.
pub const MAX_UPLOAD_BYTES: u64 = 5 * 1024 * 1024;

/// `file_type` of the upload.
pub mod file_type {
    /// png or jpg (and the other image formats QQ lists).
    pub const IMAGE: u8 = 1;
    /// mp4.
    pub const VIDEO: u8 = 2;
    /// SILK only.
    pub const VOICE: u8 = 3;
    /// Any other file.
    pub const FILE: u8 = 4;
}

/// `msg_type` of a message carrying uploaded media.
pub const MEDIA_MESSAGE: u8 = 7;

/// Image types QQ lists for `file_type` 1.
const IMAGE_TYPES: &[&str] = &[
    "image/jpeg",
    "image/jpg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/bmp",
];

/// The `file_type` of a file sent as `kind`: video only as mp4 and voice
/// only as SILK, since QQ plays nothing else; anything else goes as a file.
#[must_use]
pub fn upload_file_type(kind: MediaKind, mime_type: Option<&str>, filename: Option<&str>) -> u8 {
    let essence = mime_type
        .and_then(|mime| mime.split(';').next())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let extension = filename
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_stem, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match kind {
        MediaKind::Image
            if IMAGE_TYPES.contains(&essence.as_str())
                || matches!(
                    extension.as_str(),
                    "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
                ) =>
        {
            file_type::IMAGE
        }
        MediaKind::Video if essence == "video/mp4" || extension == "mp4" => file_type::VIDEO,
        MediaKind::Audio
            if essence == "audio/silk" || matches!(extension.as_str(), "silk" | "slk") =>
        {
            file_type::VOICE
        }
        _ => file_type::FILE,
    }
}

/// The name `file_name` carries: the sender's, else one from the kind and
/// MIME type.
#[must_use]
pub fn upload_file_name(
    kind: MediaKind,
    mime_type: Option<&str>,
    filename: Option<&str>,
) -> String {
    if let Some(name) = filename.map(str::trim).filter(|name| !name.is_empty()) {
        return name.into();
    }
    let stem = match kind {
        MediaKind::File => "file",
        MediaKind::Image => "image",
        MediaKind::Audio => "audio",
        MediaKind::Video => "video",
    };
    format!("{stem}.{}", mime_extension(mime_type.unwrap_or_default()))
}

/// Bytes of the base64 of `len` bytes, with padding.
#[must_use]
pub const fn base64_len(len: u64) -> u64 {
    len.div_ceil(3).saturating_mul(4)
}

/// The content of a send, re-readable when it can be, so a request QQ
/// refused with 401 can go again with a new token.
pub(crate) enum Content {
    Bytes(Rc<Vec<u8>>),
    Source(Rc<dyn MediaSource>),
    /// Read once; `None` once it has been.
    Stream(Option<BinaryStream>),
}

impl Content {
    /// The content of `body` and its length; a stream's length is `size`.
    pub(crate) fn new(body: BinaryBody, size: Option<u64>) -> Result<(Self, u64), ChannelError> {
        match body {
            BinaryBody::Bytes(bytes) => {
                let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                Ok((Self::Bytes(Rc::new(bytes)), len))
            }
            BinaryBody::Source(source) => {
                let len = source.len();
                Ok((Self::Source(source), len))
            }
            BinaryBody::Stream(stream) => {
                let len = size.ok_or_else(|| ChannelError::InvalidRequest {
                    message: "QQ uploads need the file's length; send it by path".into(),
                })?;
                Ok((Self::Stream(Some(stream)), len))
            }
        }
    }

    /// A stream from the first byte, or `None` for a stream already read.
    pub(crate) async fn open(&mut self) -> Result<Option<BinaryStream>, ChannelError> {
        match self {
            Self::Bytes(bytes) => {
                let bytes = Rc::clone(bytes);
                Ok(Some(Box::pin(stream::once(Ok(BinaryChunk::from(
                    bytes.as_ref().clone(),
                ))))))
            }
            Self::Source(source) => Ok(Some(source.open().await?)),
            Self::Stream(stream) => Ok(stream.take()),
        }
    }
}

/// The upload body: `envelope` (a JSON object without its closing brace and
/// `file_data`), then `,"file_data":"<base64 of content>"}`, as a stream.
///
/// The returned length is exact for content of `len` bytes.
pub(crate) fn upload_body(
    envelope: String,
    content: BinaryStream,
    len: u64,
) -> (BinaryStream, u64) {
    let head = format!("{envelope},\"file_data\":\"").into_bytes();
    let length = u64::try_from(head.len())
        .unwrap_or(u64::MAX)
        .saturating_add(base64_len(len))
        .saturating_add(2);
    let encoded = Base64Stream {
        content: Some(content),
        carry: Vec::new(),
    };
    let body = stream::once(Ok(BinaryChunk::from(head)))
        .chain(stream::unfold(encoded, |mut encoded| async move {
            let chunk = encoded.next().await?;
            Some((chunk, encoded))
        }))
        .chain(stream::once(Ok(BinaryChunk::from(b"\"}".to_vec()))));
    (Box::pin(body), length)
}

/// Base64 of a byte stream, emitted in whole groups of three input bytes
/// until the stream ends.
struct Base64Stream {
    content: Option<BinaryStream>,
    /// Up to two input bytes waiting for the rest of their group.
    carry: Vec<u8>,
}

impl Base64Stream {
    async fn next(&mut self) -> Option<Result<BinaryChunk, StreamError>> {
        let engine = base64::engine::general_purpose::STANDARD;
        loop {
            let content = self.content.as_mut()?;
            match content.next().await {
                Some(Ok(bytes)) => {
                    self.carry.extend_from_slice(&bytes);
                    let whole = self.carry.len().saturating_sub(self.carry.len() % 3);
                    if whole == 0 {
                        continue;
                    }
                    let rest = self.carry.split_off(whole);
                    let encoded = engine.encode(&self.carry);
                    self.carry = rest;
                    return Some(Ok(BinaryChunk::from(encoded.into_bytes())));
                }
                Some(Err(error)) => {
                    self.content = None;
                    return Some(Err(error));
                }
                None => {
                    self.content = None;
                    if self.carry.is_empty() {
                        return None;
                    }
                    let encoded = engine.encode(&self.carry);
                    self.carry.clear();
                    return Some(Ok(BinaryChunk::from(encoded.into_bytes())));
                }
            }
        }
    }
}

/// The `file_info` of an upload answer.
pub(crate) fn file_info(root: &serde_json::Value) -> Result<String, ChannelError> {
    root.get("file_info")
        .and_then(serde_json::Value::as_str)
        .filter(|info| !info.is_empty())
        .map(String::from)
        .ok_or_else(|| ChannelError::Platform {
            code: None,
            message: "QQ upload answer omitted file_info".into(),
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use futures_lite::future::block_on;

    fn collect(mut body: BinaryStream) -> Vec<u8> {
        block_on(async {
            let mut bytes = Vec::new();
            while let Some(chunk) = body.next().await {
                bytes.extend_from_slice(&chunk.expect("chunk"));
            }
            bytes
        })
    }

    #[test]
    fn the_upload_body_is_json_with_the_content_in_base64_and_an_exact_length() {
        for (pieces, expected) in [
            (&[&b""[..]][..], ""),
            (&[&b"A"[..]][..], "QQ=="),
            (&[&b"AB"[..], b"C"][..], "QUJD"),
            (&[&b"A"[..], b"B", b"CD"][..], "QUJDRA=="),
            (&[&b"hello world"[..]][..], "aGVsbG8gd29ybGQ="),
        ] {
            let len: usize = pieces.iter().map(|piece| piece.len()).sum();
            let chunks: Vec<_> = pieces
                .iter()
                .map(|piece| Ok(BinaryChunk::from(piece.to_vec())))
                .collect();
            let (body, length) = upload_body(
                String::from(r#"{"file_type":1"#),
                Box::pin(stream::iter(chunks)),
                len as u64,
            );
            let bytes = collect(body);
            assert_eq!(bytes.len() as u64, length);
            let json: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
            assert_eq!(json["file_type"], 1);
            assert_eq!(json["file_data"], expected);
        }
    }

    #[test]
    fn file_types_follow_what_qq_plays() {
        use file_type::{FILE, IMAGE, VIDEO, VOICE};
        assert_eq!(
            upload_file_type(MediaKind::Image, Some("image/png"), None),
            IMAGE
        );
        assert_eq!(
            upload_file_type(MediaKind::Image, None, Some("a.JPG")),
            IMAGE
        );
        assert_eq!(
            upload_file_type(MediaKind::Image, Some("image/heic"), None),
            FILE
        );
        assert_eq!(
            upload_file_type(MediaKind::Video, Some("video/mp4"), None),
            VIDEO
        );
        assert_eq!(
            upload_file_type(MediaKind::Video, Some("video/quicktime"), None),
            FILE
        );
        assert_eq!(
            upload_file_type(MediaKind::Audio, Some("audio/silk"), None),
            VOICE
        );
        assert_eq!(
            upload_file_type(MediaKind::Audio, None, Some("v.slk")),
            VOICE
        );
        assert_eq!(
            upload_file_type(MediaKind::Audio, Some("audio/ogg"), Some("v.ogg")),
            FILE
        );
        assert_eq!(
            upload_file_type(MediaKind::File, Some("image/png"), None),
            FILE
        );
        assert_eq!(
            upload_file_name(MediaKind::Audio, Some("audio/ogg"), None),
            "audio.ogg"
        );
        assert_eq!(
            upload_file_name(MediaKind::File, None, Some(" a.pdf ")),
            "a.pdf"
        );
    }
}

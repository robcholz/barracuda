//! Files on the iLink CDN: the media items of a message, their keys and
//! URLs, and the encrypted upload of a file to send.
//!
//! Every file on the CDN is AES-128-ECB encrypted with PKCS#7 padding under a
//! key of its own. An inbound item names the file by `encrypt_query_param`
//! (or a `full_url`) and carries the key: `image_item.aeskey` as hex, else
//! `media.aes_key` as base64 of the 16 raw bytes or of their 32 hex digits.
//! To send, the bot asks `getuploadurl` for an upload parameter with the
//! file's plaintext MD5 and sizes, posts the ciphertext to the CDN with a
//! `Content-Length`, takes the download parameter from the
//! `x-encrypted-param` response header, and sends a message whose item
//! names it with the key (`aes_key` as base64 of the hex digits).
//!
//! The protocol is the one Tencent's own iLink client implements
//! (`@tencent-weixin/openclaw-weixin` 2.4.10: `src/api/types.ts`,
//! `src/cdn/*.ts`, `src/media/media-download.ts`, `src/messaging/send.ts`).

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt::Write as _;

use barracuda_imessage_gateway_plugin::{
    extension_mime, BinaryChunk, BinaryStream, ChannelError, MediaKind, StreamError,
};
use barracuda_tls::{Aes128EcbEncryptor, AES_128_KEY_LEN};
use base64::Engine as _;
use futures_lite::{stream, StreamExt as _};
use serde::Deserialize;
use serde_json::{json, Value};

/// CDN that iLink files live on, unless an item names a `full_url`.
pub const DEFAULT_CDN_BASE: &str = "https://novac2c.cdn.weixin.qq.com/c2c";

/// `item_list[].type` of each item kind.
pub mod item_type {
    /// Text.
    pub const TEXT: u32 = 1;
    /// Image.
    pub const IMAGE: u32 = 2;
    /// Voice message.
    pub const VOICE: u32 = 3;
    /// File.
    pub const FILE: u32 = 4;
    /// Video.
    pub const VIDEO: u32 = 5;
}

/// `media_type` of a `getuploadurl` request.
pub mod upload_type {
    /// Image.
    pub const IMAGE: u8 = 1;
    /// Video.
    pub const VIDEO: u8 = 2;
    /// File.
    pub const FILE: u8 = 3;
}

/// A CDN reference of an inbound item.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct CdnMedia {
    /// Download parameter of the file.
    #[serde(default)]
    pub encrypt_query_param: Option<String>,
    /// The file's key; see [`parse_aes_key`].
    #[serde(default)]
    pub aes_key: Option<String>,
    /// Complete download URL, when iLink names one.
    #[serde(default)]
    pub full_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawImage {
    #[serde(default)]
    media: Option<CdnMedia>,
    /// The key as 32 hex digits; preferred over `media.aes_key`.
    #[serde(default)]
    aeskey: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawVoice {
    #[serde(default)]
    media: Option<CdnMedia>,
    /// 5 amr, 6 silk, 7 mp3; others are raw or speex codecs.
    #[serde(default)]
    encode_type: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawFile {
    #[serde(default)]
    media: Option<CdnMedia>,
    #[serde(default)]
    file_name: Option<String>,
    /// Plaintext length, as a decimal string.
    #[serde(default)]
    len: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawVideo {
    #[serde(default)]
    media: Option<CdnMedia>,
}

/// One file of an inbound message, ready to download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundMedia {
    /// How the file is presented.
    pub kind: MediaKind,
    /// Where the file is, when the item named it.
    pub media: Option<CdnMedia>,
    /// The file's key; `None` for an image sent unencrypted, or an item
    /// without a usable key.
    pub key: Option<[u8; AES_128_KEY_LEN]>,
    /// MIME type the item implies; `None` leaves it to the content.
    pub mime_type: Option<String>,
    /// Name the sender gave a file.
    pub filename: Option<String>,
    /// Plaintext length a file item declares.
    pub size: Option<u64>,
}

impl InboundMedia {
    /// Whether the file can be fetched: a reference, and a key unless it is
    /// an image (which may be stored unencrypted).
    #[must_use]
    pub fn fetchable(&self) -> bool {
        let named = self.media.as_ref().is_some_and(|media| {
            media.full_url.as_deref().is_some_and(|url| !url.is_empty())
                || media
                    .encrypt_query_param
                    .as_deref()
                    .is_some_and(|param| !param.is_empty())
        });
        named && (self.key.is_some() || self.kind == MediaKind::Image)
    }
}

pub(crate) fn image(item: RawImage) -> InboundMedia {
    let key = item
        .aeskey
        .as_deref()
        .and_then(hex_key)
        .or_else(|| item_key(item.media.as_ref()));
    InboundMedia {
        kind: MediaKind::Image,
        media: item.media,
        key,
        mime_type: None,
        filename: None,
        size: None,
    }
}

pub(crate) fn voice(item: RawVoice) -> InboundMedia {
    let mime = match item.encode_type {
        Some(5) => "audio/amr",
        Some(7) => "audio/mpeg",
        // iLink's client treats every voice note as SILK.
        _ => "audio/silk",
    };
    InboundMedia {
        kind: MediaKind::Audio,
        key: item_key(item.media.as_ref()),
        media: item.media,
        mime_type: Some(mime.into()),
        filename: None,
        size: None,
    }
}

pub(crate) fn file(item: RawFile) -> InboundMedia {
    let filename = item.file_name.filter(|name| !name.trim().is_empty());
    InboundMedia {
        kind: MediaKind::File,
        key: item_key(item.media.as_ref()),
        media: item.media,
        mime_type: filename
            .as_deref()
            .and_then(extension_mime)
            .map(String::from),
        filename,
        size: item.len.and_then(|len| len.trim().parse().ok()),
    }
}

pub(crate) fn video(item: RawVideo) -> InboundMedia {
    InboundMedia {
        kind: MediaKind::Video,
        key: item_key(item.media.as_ref()),
        media: item.media,
        mime_type: Some("video/mp4".into()),
        filename: None,
        size: None,
    }
}

fn item_key(media: Option<&CdnMedia>) -> Option<[u8; AES_128_KEY_LEN]> {
    parse_aes_key(media?.aes_key.as_deref()?)
}

/// The 16-byte key a `media.aes_key` carries: base64 of the raw key, or
/// base64 of its 32 hex digits.
#[must_use]
pub fn parse_aes_key(aes_key: &str) -> Option<[u8; AES_128_KEY_LEN]> {
    let decoded = base64_decode(aes_key.trim())?;
    if let Ok(key) = <[u8; AES_128_KEY_LEN]>::try_from(decoded.as_slice()) {
        return Some(key);
    }
    core::str::from_utf8(&decoded).ok().and_then(hex_key)
}

/// A key written as 32 hex digits.
fn hex_key(hex: &str) -> Option<[u8; AES_128_KEY_LEN]> {
    let hex = hex.trim().as_bytes();
    if hex.len() != AES_128_KEY_LEN * 2 {
        return None;
    }
    let mut key = [0_u8; AES_128_KEY_LEN];
    for (byte, pair) in key.iter_mut().zip(hex.chunks(2)) {
        let pair = core::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(key)
}

/// Lowercase hex digits of `bytes`.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _infallible = write!(text, "{byte:02x}");
    }
    text
}

/// Where to download `media`: its `full_url`, else
/// `<cdn_base>/download?encrypted_query_param=<param>`. The URL is a
/// credential for the file: never log it.
#[must_use]
pub fn download_url(cdn_base: &str, media: &CdnMedia) -> Option<String> {
    if let Some(url) = media.full_url.as_deref().map(str::trim) {
        if !url.is_empty() {
            return Some(url.into());
        }
    }
    let param = media.encrypt_query_param.as_deref()?;
    (!param.is_empty()).then(|| {
        format!(
            "{}/download?encrypted_query_param={}",
            cdn_base.trim_end_matches('/'),
            percent_encode(param)
        )
    })
}

/// Where to upload: `upload_full_url`, else
/// `<cdn_base>/upload?encrypted_query_param=<param>&filekey=<filekey>`.
pub(crate) fn upload_url(cdn_base: &str, answer: &UploadUrl, filekey: &str) -> Option<String> {
    if let Some(url) = answer.upload_full_url.as_deref().map(str::trim) {
        if !url.is_empty() {
            return Some(url.into());
        }
    }
    let param = answer.upload_param.as_deref()?;
    (!param.is_empty()).then(|| {
        format!(
            "{}/upload?encrypted_query_param={}&filekey={}",
            cdn_base.trim_end_matches('/'),
            percent_encode(param),
            percent_encode(filekey)
        )
    })
}

/// `getuploadurl` answer.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct UploadUrl {
    #[serde(default)]
    pub(crate) upload_param: Option<String>,
    #[serde(default)]
    pub(crate) upload_full_url: Option<String>,
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _infallible = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// The `getuploadurl` `media_type` of a file sent as `kind`: audio has no
/// item of its own on the way out, so it goes as a file.
#[must_use]
pub const fn upload_media_type(kind: MediaKind) -> u8 {
    match kind {
        MediaKind::Image => upload_type::IMAGE,
        MediaKind::Video => upload_type::VIDEO,
        MediaKind::File | MediaKind::Audio => upload_type::FILE,
    }
}

/// What a finished upload tells the message that sends it.
pub(crate) struct Uploaded {
    pub(crate) download_param: String,
    pub(crate) key: [u8; AES_128_KEY_LEN],
    pub(crate) plaintext_len: u64,
    pub(crate) ciphertext_len: u64,
}

/// The message item naming an uploaded file.
pub(crate) fn sent_item(kind: MediaKind, uploaded: &Uploaded, file_name: &str) -> Value {
    let media = json!({
        "encrypt_query_param": uploaded.download_param,
        // iLink's client sends base64 of the hex digits.
        "aes_key": base64_encode(hex(&uploaded.key).as_bytes()),
        "encrypt_type": 1,
    });
    match kind {
        MediaKind::Image => json!({
            "type": item_type::IMAGE,
            "image_item": { "media": media, "mid_size": uploaded.ciphertext_len },
        }),
        MediaKind::Video => json!({
            "type": item_type::VIDEO,
            "video_item": { "media": media, "video_size": uploaded.ciphertext_len },
        }),
        MediaKind::File | MediaKind::Audio => json!({
            "type": item_type::FILE,
            "file_item": {
                "media": media,
                "file_name": file_name,
                "len": uploaded.plaintext_len.to_string(),
            },
        }),
    }
}

/// `content` read through AES-128-ECB with `key`, padded at its end.
pub(crate) fn encrypting(content: BinaryStream, key: &[u8; AES_128_KEY_LEN]) -> BinaryStream {
    let state = (content, Aes128EcbEncryptor::new(key).ok(), false);
    Box::pin(stream::unfold(
        state,
        |(mut content, mut encryptor, done)| async move {
            if done {
                return None;
            }
            loop {
                let Some(cipher) = encryptor.as_mut() else {
                    let failed = Err(StreamError::failed("AES engine unavailable"));
                    return Some((failed, (content, None, true)));
                };
                match content.next().await {
                    Some(Ok(bytes)) => {
                        let mut sealed = Vec::with_capacity(bytes.len().saturating_add(16));
                        if cipher.update(&bytes, &mut sealed).is_err() {
                            let failed = Err(StreamError::failed("AES encryption failed"));
                            return Some((failed, (content, None, true)));
                        }
                        if !sealed.is_empty() {
                            return Some((
                                Ok(BinaryChunk::from(sealed)),
                                (content, encryptor, false),
                            ));
                        }
                    }
                    Some(Err(error)) => return Some((Err(error), (content, None, true))),
                    None => {
                        let mut sealed = Vec::with_capacity(16);
                        let finished = encryptor
                            .take()
                            .ok_or(())
                            .and_then(|cipher| cipher.finish(&mut sealed).map_err(|_error| ()));
                        let chunk = match finished {
                            Ok(()) => Ok(BinaryChunk::from(sealed)),
                            Err(()) => Err(StreamError::failed("AES encryption failed")),
                        };
                        return Some((chunk, (content, None, true)));
                    }
                }
            }
        },
    ))
}

/// Standard base64 with padding.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decodes standard base64, padded or not.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(text.trim_end_matches('='))
        .ok()
}

/// The image type of `bytes` by its signature, for an image item, which
/// names none.
#[must_use]
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("image/webp")
    } else {
        None
    }
}

/// The error of a failed cipher setup or digest.
pub(crate) fn crypto_error(error: barracuda_tls::CryptoError) -> ChannelError {
    ChannelError::Stream(StreamError::failed(error.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use futures_lite::future::block_on;

    const KEY: [u8; 16] = *b"0123456789abcdef";

    #[test]
    fn keys_come_as_hex_or_base64_of_raw_bytes_or_of_hex() {
        let hex_digits = hex(&KEY);
        assert_eq!(hex_digits, "30313233343536373839616263646566");
        assert_eq!(hex_key(&hex_digits), Some(KEY));
        assert_eq!(parse_aes_key(&base64_encode(&KEY)), Some(KEY));
        assert_eq!(
            parse_aes_key(&base64_encode(hex_digits.as_bytes())),
            Some(KEY)
        );
        assert_eq!(parse_aes_key("MTIz"), None);
        assert_eq!(parse_aes_key("not base64!"), None);
        assert_eq!(
            base64_decode("aGVsbG8gd29ybGQ").as_deref(),
            Some(&b"hello world"[..])
        );
    }

    #[test]
    fn urls_prefer_the_full_url_and_escape_parameters() {
        let media = CdnMedia {
            encrypt_query_param: Some("a+b/c=".into()),
            ..CdnMedia::default()
        };
        assert_eq!(
            download_url("https://cdn.test/c2c/", &media).as_deref(),
            Some("https://cdn.test/c2c/download?encrypted_query_param=a%2Bb%2Fc%3D")
        );
        let full = CdnMedia {
            full_url: Some("https://cdn.test/full".into()),
            ..media
        };
        assert_eq!(
            download_url("x", &full).as_deref(),
            Some("https://cdn.test/full")
        );
        assert_eq!(download_url("x", &CdnMedia::default()), None);
        let answer = UploadUrl {
            upload_param: Some("p=1".into()),
            upload_full_url: Some(" ".into()),
        };
        assert_eq!(
            upload_url("https://cdn.test/c2c", &answer, "fk").as_deref(),
            Some("https://cdn.test/c2c/upload?encrypted_query_param=p%3D1&filekey=fk")
        );
    }

    #[test]
    fn encrypting_a_stream_matches_encrypting_it_whole() {
        let plaintext: Vec<u8> = (0..100_u8).collect();
        let chunks: Vec<_> = plaintext
            .chunks(7)
            .map(|chunk| Ok(BinaryChunk::from(chunk.to_vec())))
            .collect();
        let mut sealed = encrypting(Box::pin(stream::iter(chunks)), &KEY);
        let bytes = block_on(async {
            let mut bytes = Vec::new();
            while let Some(chunk) = sealed.next().await {
                bytes.extend_from_slice(&chunk.expect("chunk"));
            }
            bytes
        });
        assert_eq!(
            bytes,
            barracuda_tls::aes_128_ecb_encrypt(&KEY, &plaintext).expect("encrypt")
        );
    }

    #[test]
    fn images_are_recognized_by_their_signature() {
        assert_eq!(sniff_image(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image(b"GIF89a"), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"BM"), None);
    }
}

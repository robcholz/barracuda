//! The page's file routes: `PUT /api/web/upload/<mime>/<filename>` stores a
//! file the next message frame may send, and `GET /api/web/media/<id>`
//! streams a file the channel sent.
//!
//! An upload carries its type and name in the path, percent-encoded, because
//! an upload endpoint sees no headers. Its body streams through a one-chunk
//! pipe into the Gateway's attachment storage; nothing is collected in memory.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use barracuda_imessage_gateway_plugin::{
    Attachment, AttachmentMeta, BinaryChunk, BinaryStream, IMessageGateway, MediaKind, StoreError,
    StreamError,
};
use barracuda_webserver_plugin::{
    HttpFuture, HttpMethod, HttpProvider, HttpResponse, HttpUpload, HttpUploadEndpoint,
    WebRouteRegistration, WebServer, WebServerError,
};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embedded_io_async::{ErrorKind, ErrorType, Read};
use futures_lite::{future, stream, StreamExt as _};
use serde::Serialize;
use web::{KeptMedia, MEDIA_PATH};

use crate::{WebChannel, WEB_CHANNEL};

const UPLOAD_PATH: &str = "/api/web/upload/";
const JSON: &str = "application/json";
/// Bytes read from the request for one chunk of an upload.
const UPLOAD_CHUNK: usize = 2048;
const MAX_MIME: usize = 127;
const MAX_NAME: usize = 255;

/// One chunk in flight between the request body and the store, `None` at
/// the end of the body.
type Pipe = Channel<NoopRawMutex, Option<Result<BinaryChunk, StreamError>>, 1>;

/// Registers both routes; dropping the registrations removes them.
pub(crate) fn register(
    webserver: &WebServer,
    web: Rc<WebChannel>,
    gateway: Rc<IMessageGateway>,
) -> Result<[WebRouteRegistration; 2], WebServerError> {
    Ok([
        webserver.serve_upload(
            UPLOAD_PATH,
            UploadRoute {
                web: Rc::clone(&web),
                gateway,
            },
        )?,
        webserver.serve("/api/web/media/*", MediaRoute(web))?,
    ])
}

/// `PUT /api/web/upload/<mime>/<filename>`: `201 {"id","kind","mime_type","filename","size"}`.
struct UploadRoute {
    web: Rc<WebChannel>,
    gateway: Rc<IMessageGateway>,
}

impl HttpUploadEndpoint for UploadRoute {
    fn handle<'a>(&'a self, mut upload: HttpUpload<'a>) -> HttpFuture<'a> {
        Box::pin(async move {
            if upload.method() != HttpMethod::Put {
                return refused(405, "method_not_allowed");
            }
            let Some((mime_type, filename)) = upload
                .path()
                .strip_prefix(UPLOAD_PATH)
                .and_then(upload_target)
            else {
                return refused(400, "invalid_path");
            };
            let length = upload.content_length();
            // an upload endpoint cannot tell a missing length from an empty body
            if length == 0 {
                return refused(411, "length_required");
            }
            let limit = self.gateway.attachment_max_bytes();
            if u64::try_from(length).map_or(true, |length| length > limit) {
                return too_large(limit);
            }
            let meta = AttachmentMeta {
                kind: kind_of(&mime_type),
                mime_type: Some(mime_type),
                filename: Some(filename),
            };
            match self.store(meta, &mut upload, length).await {
                Ok(attachment) => {
                    let id = self.web.media().hold(attachment.clone());
                    log::info!(
                        "IMessage Web stored a {:?} upload ({} bytes) as `{id}`",
                        attachment.kind,
                        attachment.size
                    );
                    json(
                        201,
                        &Uploaded {
                            id: &id,
                            kind: attachment.kind,
                            mime_type: &attachment.mime_type,
                            filename: attachment.filename.as_deref(),
                            size: attachment.size,
                        },
                    )
                }
                Err(StoreError::TooLarge { limit }) => too_large(limit),
                Err(StoreError::NoSpace) => {
                    log::warn!("IMessage Web refused an upload: the attachment cache is full");
                    refused(507, "no_space")
                }
                Err(StoreError::Stream(error)) => {
                    log::warn!("IMessage Web upload ended early: {error}");
                    refused(400, "incomplete_body")
                }
                Err(StoreError::Filesystem(error)) => {
                    log::warn!("IMessage Web could not store an upload: {error}");
                    refused(500, "io")
                }
            }
        })
    }
}

impl UploadRoute {
    /// Streams exactly `length` bytes of `body` into the Gateway's storage.
    async fn store(
        &self,
        meta: AttachmentMeta,
        body: &mut HttpUpload<'_>,
        length: usize,
    ) -> Result<Attachment, StoreError> {
        let pipe = Rc::new(Pipe::new());
        let chunks: BinaryStream = Box::pin(stream::unfold(Rc::clone(&pipe), |pipe| async move {
            let item = pipe.receive().await?;
            Some((item, pipe))
        }));
        let feed = async {
            feed(body, &pipe, length).await;
            // the store answers once it has the end of the body
            future::pending().await
        };
        // a store that fails early (too large, no space) drops the feed with it
        future::or(self.gateway.store_inbound(WEB_CHANNEL, meta, chunks), feed).await
    }
}

/// Copies `length` bytes of `body` into `pipe`, then its end, or a failure
/// when the body ends early.
async fn feed(body: &mut HttpUpload<'_>, pipe: &Pipe, length: usize) {
    let mut remaining = length;
    while remaining > 0 {
        let mut chunk = vec![0_u8; remaining.min(UPLOAD_CHUNK)];
        match body.read(&mut chunk).await {
            Ok(count) if count > 0 => {
                chunk.truncate(count);
                remaining = remaining.saturating_sub(count);
                pipe.send(Some(Ok(BinaryChunk::from(chunk)))).await;
            }
            Ok(_) | Err(_) => {
                pipe.send(Some(Err(StreamError::failed(
                    "upload ended before its length",
                ))))
                .await;
                return;
            }
        }
    }
    pipe.send(None).await;
}

#[derive(Serialize)]
struct Uploaded<'a> {
    id: &'a str,
    kind: MediaKind,
    mime_type: &'a str,
    filename: Option<&'a str>,
    size: u64,
}

/// The MIME type and file name an upload path names after its prefix.
fn upload_target(encoded: &str) -> Option<(String, String)> {
    let (mime, name) = encoded.split_once('/')?;
    let mime = decode(mime)?.to_ascii_lowercase();
    let name = decode(name)?;
    (valid_mime(&mime) && valid_name(&name)).then_some((mime, name))
}

/// `type/subtype`, both made of MIME token characters, without parameters.
fn valid_mime(mime: &str) -> bool {
    let token = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&byte))
    };
    mime.len() <= MAX_MIME
        && mime
            .split_once('/')
            .is_some_and(|(kind, subtype)| token(kind) && token(subtype))
}

/// A file name: non-empty, without separators or control characters.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|character| character == '/' || character == '\\' || character.is_control())
}

/// How a file of `mime` is presented: a picture only for the types a page
/// shows as one.
fn kind_of(mime: &str) -> MediaKind {
    match mime.split_once('/') {
        Some(("image", "jpeg" | "png" | "gif" | "webp")) => MediaKind::Image,
        Some(("audio", _)) => MediaKind::Audio,
        Some(("video", _)) => MediaKind::Video,
        _ => MediaKind::File,
    }
}

/// Decodes one percent-encoded path segment as UTF-8.
fn decode(encoded: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut bytes = encoded.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = hex(bytes.next()?)?;
            let low = hex(bytes.next()?)?;
            decoded.push((high << 4) | low);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).ok()
}

const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `GET /api/web/media/<id>`: the file's bytes, streamed.
struct MediaRoute(Rc<WebChannel>);

impl HttpProvider for MediaRoute {
    async fn serve(&self, path: &str) -> HttpResponse {
        let Some(media) = path
            .strip_prefix(MEDIA_PATH)
            .and_then(|id| self.0.media().kept(id))
        else {
            return refused(404, "not_found");
        };
        let KeptMedia {
            source, mime_type, ..
        } = media;
        let Ok(length) = usize::try_from(source.len()) else {
            return refused(500, "io");
        };
        match source.open().await {
            Ok(stream) => HttpResponse::stream(
                200,
                content_type(&mime_type),
                length,
                StreamReader {
                    stream,
                    chunk: Vec::new(),
                    offset: 0,
                },
            ),
            // the file left the cache: to the page it is as gone as an evicted id
            Err(error) => {
                log::warn!("IMessage Web could not open kept media: {error}");
                refused(404, "not_found")
            }
        }
    }
}

/// Files are served under the portal's origin, so only types a browser shows
/// without running anything keep their type: pictures, audio and video. All
/// else, HTML and SVG included, is bytes.
fn content_type(mime_type: &str) -> &'static str {
    let essence = mime_type.split(';').next().unwrap_or_default().trim();
    let known: &[(&str, &'static str)] = &[
        ("image/jpeg", "image/jpeg"),
        ("image/jpg", "image/jpeg"),
        ("image/pjpeg", "image/jpeg"),
        ("image/png", "image/png"),
        ("image/gif", "image/gif"),
        ("image/webp", "image/webp"),
        ("audio/mpeg", "audio/mpeg"),
        ("audio/mp3", "audio/mpeg"),
        ("audio/mp4", "audio/mp4"),
        ("audio/m4a", "audio/mp4"),
        ("audio/x-m4a", "audio/mp4"),
        ("audio/aac", "audio/aac"),
        ("audio/ogg", "audio/ogg"),
        ("application/ogg", "audio/ogg"),
        // an `.opus` file is Ogg Opus
        ("audio/opus", "audio/ogg"),
        ("audio/wav", "audio/wav"),
        ("audio/x-wav", "audio/wav"),
        ("audio/wave", "audio/wav"),
        ("audio/webm", "audio/webm"),
        ("audio/flac", "audio/flac"),
        ("video/mp4", "video/mp4"),
        ("video/webm", "video/webm"),
        ("video/quicktime", "video/quicktime"),
        ("video/ogg", "video/ogg"),
    ];
    known
        .iter()
        .find(|(mime, _served)| mime.eq_ignore_ascii_case(essence))
        .map_or("application/octet-stream", |&(_mime, served)| served)
}

/// Reads a media stream chunk by chunk.
struct StreamReader {
    stream: BinaryStream,
    chunk: Vec<u8>,
    offset: usize,
}

impl ErrorType for StreamReader {
    type Error = ErrorKind;
}

impl Read for StreamReader {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, ErrorKind> {
        while self.offset >= self.chunk.len() {
            match self.stream.next().await {
                Some(Ok(chunk)) => {
                    self.chunk = chunk.into_vec();
                    self.offset = 0;
                }
                Some(Err(_error)) => return Err(ErrorKind::Other),
                None => return Ok(0),
            }
        }
        let rest = self.chunk.get(self.offset..).unwrap_or_default();
        let count = rest.len().min(buffer.len());
        if let (Some(target), Some(source)) = (buffer.get_mut(..count), rest.get(..count)) {
            target.copy_from_slice(source);
        }
        self.offset = self.offset.saturating_add(count);
        Ok(count)
    }
}

fn too_large(limit: u64) -> HttpResponse {
    HttpResponse::new(
        413,
        JSON,
        alloc::format!(r#"{{"error":"too_large","limit":{limit}}}"#).into_bytes(),
    )
}

fn refused(status: u16, code: &str) -> HttpResponse {
    HttpResponse::new(
        status,
        JSON,
        alloc::format!(r#"{{"error":"{code}"}}"#).into_bytes(),
    )
}

fn json<T: Serialize>(status: u16, value: &T) -> HttpResponse {
    match serde_json::to_vec(value) {
        Ok(body) => HttpResponse::new(status, JSON, body),
        Err(error) => {
            log::error!("IMessage Web could not encode a response: {error}");
            refused(500, "encoding")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{content_type, kind_of, upload_target};
    use barracuda_imessage_gateway_plugin::MediaKind;

    #[test]
    fn an_upload_path_names_a_mime_type_and_a_file_name() {
        assert_eq!(
            upload_target("image%2FPNG/cat%20photo.png"),
            Some(("image/png".into(), "cat photo.png".into()))
        );
        for rejected in [
            "image%2Fpng",
            "image%2Fpng/",
            "image/png/a.png",
            "notamime/a.png",
            "image%2Fpng%3B%20q%3D1/a.png",
            "image%2Fpng/a%2Fb.png",
            "image%2Fpng/..",
            "image%2Fpng/a%00",
            "image%2Fpng/%E2%9C",
            "image%2Fpng/%zz",
        ] {
            assert_eq!(upload_target(rejected), None, "{rejected}");
        }
    }

    #[test]
    fn only_pictures_audio_and_video_keep_their_type() {
        assert_eq!(kind_of("image/png"), MediaKind::Image);
        assert_eq!(kind_of("image/svg+xml"), MediaKind::File);
        assert_eq!(kind_of("audio/webm"), MediaKind::Audio);
        assert_eq!(kind_of("video/mp4"), MediaKind::Video);
        assert_eq!(kind_of("application/pdf"), MediaKind::File);
        assert_eq!(content_type("IMAGE/JPEG"), "image/jpeg");
        assert_eq!(content_type("audio/ogg; codecs=opus"), "audio/ogg");
        assert_eq!(content_type("video/quicktime"), "video/quicktime");
        assert_eq!(content_type("text/html"), "application/octet-stream");
        assert_eq!(content_type("image/svg+xml"), "application/octet-stream");
        assert_eq!(content_type("text/plain"), "application/octet-stream");
    }
}

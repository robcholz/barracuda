//! The opening handshake (RFC 6455 section 4): an HTTP/1.1 Upgrade request
//! written by hand and the `101 Switching Protocols` answer checked.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use embedded_io_async::{Read, Write};
use sha1::{Digest as _, Sha1};

use crate::error::{HandshakeError, WsError};
use crate::reader::{Limits, WsReader};
use crate::writer::{MaskSource, WsWriter};

/// GUID every server appends to the key before hashing it.
const ACCEPT_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Length of a base64-encoded 16-byte key.
const KEY_LEN: usize = 24;

/// Length of a base64-encoded SHA-1 digest.
const ACCEPT_LEN: usize = 28;

/// The parts of a `ws://` or `wss://` URL the handshake uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Url<'a> {
    /// Host and, when the URL names one, `:port`, as sent in `Host`.
    pub host: &'a str,
    /// Path and query, at least `/`.
    pub path: &'a str,
    /// Whether the URL asks for TLS (`wss://`).
    pub tls: bool,
}

impl<'a> Url<'a> {
    /// Splits a `ws://` or `wss://` URL (case-insensitive scheme).
    ///
    /// # Errors
    ///
    /// Returns [`WsError::InvalidUrl`] for another scheme, a missing host, or
    /// user information.
    pub fn parse(url: &'a str) -> Result<Self, WsError> {
        let (scheme, rest) = url.split_once("://").ok_or(WsError::InvalidUrl)?;
        let tls = if scheme.eq_ignore_ascii_case("wss") {
            true
        } else if scheme.eq_ignore_ascii_case("ws") {
            false
        } else {
            return Err(WsError::InvalidUrl);
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let (host, path) = match rest.find(['/', '?']) {
            Some(index) => rest.split_at(index),
            None => (rest, "/"),
        };
        let path = if path.starts_with('?') {
            return Err(WsError::InvalidUrl);
        } else if path.is_empty() {
            "/"
        } else {
            path
        };
        if host.is_empty() || host.contains(['@', ' ']) || host.starts_with(':') {
            return Err(WsError::InvalidUrl);
        }
        Ok(Self { host, path, tls })
    }
}

/// What the client sends in its opening request.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    /// `Host` header value.
    pub host: &'a str,
    /// Request target.
    pub path: &'a str,
    /// Sixteen random bytes for `Sec-WebSocket-Key`, fresh per connection.
    pub key: [u8; 16],
    /// Extra headers, such as `Authorization` or `User-Agent`.
    pub headers: &'a [(&'a str, &'a str)],
}

impl<'a> Request<'a> {
    /// A request for `url` with `key` and no extra headers.
    #[must_use]
    pub const fn new(url: Url<'a>, key: [u8; 16]) -> Self {
        Self {
            host: url.host,
            path: url.path,
            key,
            headers: &[],
        }
    }
}

/// Base64 of the 16 key bytes, as sent in `Sec-WebSocket-Key`.
#[must_use]
pub fn encode_key(key: &[u8; 16]) -> [u8; KEY_LEN] {
    let mut encoded = [0_u8; KEY_LEN];
    // 16 bytes always encode to 24 characters
    let _written = STANDARD.encode_slice(key, &mut encoded);
    encoded
}

/// The `Sec-WebSocket-Accept` a server must answer for `encoded_key`.
#[must_use]
pub fn accept_key(encoded_key: &[u8]) -> [u8; ACCEPT_LEN] {
    let mut hasher = Sha1::new();
    hasher.update(encoded_key);
    hasher.update(ACCEPT_GUID);
    let digest = hasher.finalize();
    let mut encoded = [0_u8; ACCEPT_LEN];
    // 20 bytes always encode to 28 characters
    let _written = STANDARD.encode_slice(digest, &mut encoded);
    encoded
}

/// Performs the opening handshake over `reader` and `writer` and returns
/// the two halves of the WebSocket.
///
/// Bytes the server sends right after its headers (often its first frame)
/// stay buffered in the returned reader.
///
/// # Errors
///
/// Returns [`WsError::Handshake`] when the server does not upgrade or answers
/// a wrong `Sec-WebSocket-Accept`, and [`WsError::Io`] or [`WsError::Eof`]
/// when the stream fails.
///
/// # Cancel safety
///
/// Not cancel-safe: dropping it leaves the stream mid-handshake, so drop the
/// connection too.
pub async fn connect<R, W, M>(
    reader: R,
    mut writer: W,
    request: &Request<'_>,
    mask: M,
    limits: Limits,
) -> Result<(WsReader<R>, WsWriter<W, M>), WsError>
where
    R: Read,
    W: Write,
    M: MaskSource,
{
    let key = encode_key(&request.key);
    write_request(&mut writer, request, &key).await?;
    let mut reader = WsReader::new(reader, limits);
    let expected = accept_key(&key);
    reader.read_handshake(&expected).await?;
    Ok((reader, WsWriter::new(writer, mask)))
}

async fn write_request<W: Write>(
    writer: &mut W,
    request: &Request<'_>,
    key: &[u8; KEY_LEN],
) -> Result<(), WsError> {
    let parts: [&[u8]; 7] = [
        b"GET ",
        request.path.as_bytes(),
        b" HTTP/1.1\r\nHost: ",
        request.host.as_bytes(),
        b"\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: ",
        key,
        b"\r\n",
    ];
    for part in parts {
        writer
            .write_all(part)
            .await
            .map_err(|error| WsError::io(&error))?;
    }
    for (name, value) in request.headers {
        for part in [name.as_bytes(), b": ", value.as_bytes(), b"\r\n"] {
            writer
                .write_all(part)
                .await
                .map_err(|error| WsError::io(&error))?;
        }
    }
    writer
        .write_all(b"\r\n")
        .await
        .map_err(|error| WsError::io(&error))?;
    writer.flush().await.map_err(|error| WsError::io(&error))
}

/// Checks a complete response head (without the final blank line).
pub(crate) fn check_response(head: &[u8], expected_accept: &[u8]) -> Result<(), HandshakeError> {
    let head = core::str::from_utf8(head).map_err(|_| HandshakeError::Malformed)?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or(HandshakeError::Malformed)?;
    let mut status_parts = status_line.splitn(3, ' ');
    let version = status_parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(HandshakeError::Malformed);
    }
    let status = status_parts
        .next()
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or(HandshakeError::Malformed)?;
    if status != 101 {
        return Err(HandshakeError::Status(status));
    }
    let mut upgrade = false;
    let mut connection = false;
    let mut accepted = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(HandshakeError::Malformed);
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("upgrade") {
            upgrade = value.eq_ignore_ascii_case("websocket");
        } else if name.eq_ignore_ascii_case("connection") {
            connection = value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
        } else if name.eq_ignore_ascii_case("sec-websocket-accept") {
            accepted = value.as_bytes() == expected_accept;
        } else if (name.eq_ignore_ascii_case("sec-websocket-extensions")
            || name.eq_ignore_ascii_case("sec-websocket-protocol"))
            && !value.is_empty()
        {
            return Err(HandshakeError::UnexpectedExtension);
        }
    }
    if !upgrade || !connection {
        return Err(HandshakeError::NotUpgraded);
    }
    if !accepted {
        return Err(HandshakeError::BadAccept);
    }
    Ok(())
}

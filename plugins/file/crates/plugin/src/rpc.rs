use alloc::rc::Rc;
use core::fmt::{self, Write as _};

use barracuda_event_router::{
    JsonHandler, JsonPayload, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RpcError, json_schema,
};
use embedded_io_async::Read as _;
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::FileSystem;

const REQUEST_CAPACITY: usize = 512;
const READ_RESPONSE_CAPACITY: usize = 512;
const READ_RESPONSE_OVERHEAD: usize = 14;
const READ_CONTENT_CAPACITY: usize = READ_RESPONSE_CAPACITY - READ_RESPONSE_OVERHEAD;
const WRITE_RESPONSE_CAPACITY: usize = 29;

/// Filesystem operation rejection returned as a JSON response document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileRpcError {
    /// The request path is invalid.
    InvalidRequest,
    /// Stored file contents are not valid UTF-8.
    InvalidUtf8,
    /// The file was not found.
    NotFound,
    /// Access was denied or the mount is read-only.
    PermissionDenied,
    /// File contents exceed the bounded JSON RPC contract.
    TooLarge,
    /// Another filesystem operation failed.
    Io,
}

impl FileRpcError {
    const fn document(self) -> &'static str {
        match self {
            Self::InvalidRequest => r#"{"error":"invalid_request"}"#,
            Self::InvalidUtf8 => r#"{"error":"invalid_utf8"}"#,
            Self::NotFound => r#"{"error":"not_found"}"#,
            Self::PermissionDenied => r#"{"error":"permission_denied"}"#,
            Self::TooLarge => r#"{"error":"too_large"}"#,
            Self::Io => r#"{"error":"io"}"#,
        }
    }
}

impl core::fmt::Display for FileRpcError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "invalid request",
            Self::InvalidUtf8 => "file is not valid UTF-8",
            Self::NotFound => "not found",
            Self::PermissionDenied => "permission denied",
            Self::TooLarge => "file too large",
            Self::Io => "filesystem I/O failure",
        })
    }
}

/// Reads one bounded UTF-8 file.
pub struct FileRead;

impl JsonRpcSchema for FileRead {
    const ADDRESS: &'static str = "file.read";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("read", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("read", response);
    const MAX_REQUEST_BYTES: usize = REQUEST_CAPACITY;
    const MAX_RESPONSE_BYTES: usize = READ_RESPONSE_CAPACITY;
}

/// Replaces one bounded UTF-8 file.
pub struct FileWrite;

impl JsonRpcSchema for FileWrite {
    const ADDRESS: &'static str = "file.write";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("write", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("write", response);
    const MAX_REQUEST_BYTES: usize = REQUEST_CAPACITY;
    const MAX_RESPONSE_BYTES: usize = WRITE_RESPONSE_CAPACITY;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRequest<'a> {
    #[serde(borrow)]
    path: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteRequest<'a> {
    #[serde(borrow)]
    path: &'a RawValue,
    #[serde(borrow)]
    content: &'a RawValue,
}

fn validate_path(path: &str) -> Result<(), FileRpcError> {
    if path.is_empty() || path.as_bytes().contains(&0) {
        Err(FileRpcError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn map_fs(error: barracuda_vfs::FsError) -> FileRpcError {
    match error {
        barracuda_vfs::FsError::NotFound => FileRpcError::NotFound,
        barracuda_vfs::FsError::PermissionDenied | barracuda_vfs::FsError::ReadOnly => {
            FileRpcError::PermissionDenied
        }
        barracuda_vfs::FsError::InvalidPath | barracuda_vfs::FsError::InvalidInput => {
            FileRpcError::InvalidRequest
        }
        _ => FileRpcError::Io,
    }
}

struct ErrorResponse(FileRpcError);

impl JsonPayload for ErrorResponse {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        self.0.document().encoded_len()
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        self.0.document().write_json(destination)
    }
}

struct ReadResponse<'a> {
    content: &'a str,
}

impl JsonPayload for ReadResponse<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        let mut length = READ_RESPONSE_OVERHEAD;
        for character in self.content.chars() {
            let encoded = match character {
                '"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
                '\u{0000}'..='\u{001f}' => 6,
                other => other.len_utf8(),
            };
            length = length
                .checked_add(encoded)
                .ok_or(RpcError::InvalidFrameState)?;
        }
        Ok(length)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        let size = self.encoded_len()?;
        let capacity = destination.len();
        if size > capacity {
            return Err(RpcError::FrameTooLarge { size, capacity });
        }
        let mut writer = SliceWriter::new(destination);
        writer
            .write_str("{\"content\":\"")
            .map_err(|_error| RpcError::FrameTooLarge { size, capacity })?;
        write_json_string_body(&mut writer, self.content)
            .map_err(|_error| RpcError::FrameTooLarge { size, capacity })?;
        writer
            .write_str("\"}")
            .map_err(|_error| RpcError::FrameTooLarge { size, capacity })?;
        Ok(writer.written)
    }
}

fn write_json_string_body(writer: &mut impl fmt::Write, value: &str) -> fmt::Result {
    for character in value.chars() {
        match character {
            '"' => writer.write_str("\\\"")?,
            '\\' => writer.write_str("\\\\")?,
            '\n' => writer.write_str("\\n")?,
            '\r' => writer.write_str("\\r")?,
            '\t' => writer.write_str("\\t")?,
            '\u{0008}' => writer.write_str("\\b")?,
            '\u{000c}' => writer.write_str("\\f")?,
            control @ '\u{0000}'..='\u{001f}' => {
                write!(writer, "\\u{:04x}", u32::from(control))?;
            }
            other => writer.write_char(other)?,
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JsonStringError {
    InvalidJson,
    TooLong,
}

fn decode_json_string(raw: &RawValue, destination: &mut [u8]) -> Result<usize, JsonStringError> {
    let source = raw.get().as_bytes();
    let end = source
        .len()
        .checked_sub(1)
        .filter(|end| source.first() == Some(&b'"') && source.get(*end) == Some(&b'"'))
        .ok_or(JsonStringError::InvalidJson)?;
    let mut input = 1_usize;
    let mut output = 0_usize;
    while input < end {
        let byte = *source.get(input).ok_or(JsonStringError::InvalidJson)?;
        if byte != b'\\' {
            write_decoded_byte(destination, &mut output, byte)?;
            input = input.checked_add(1).ok_or(JsonStringError::InvalidJson)?;
            continue;
        }

        input = input.checked_add(1).ok_or(JsonStringError::InvalidJson)?;
        let escape = *source.get(input).ok_or(JsonStringError::InvalidJson)?;
        input = input.checked_add(1).ok_or(JsonStringError::InvalidJson)?;
        match escape {
            b'"' | b'\\' | b'/' => write_decoded_byte(destination, &mut output, escape)?,
            b'b' => write_decoded_byte(destination, &mut output, 0x08)?,
            b'f' => write_decoded_byte(destination, &mut output, 0x0c)?,
            b'n' => write_decoded_byte(destination, &mut output, b'\n')?,
            b'r' => write_decoded_byte(destination, &mut output, b'\r')?,
            b't' => write_decoded_byte(destination, &mut output, b'\t')?,
            b'u' => {
                let first = decode_hex_quad(source, input, end)?;
                input = input.checked_add(4).ok_or(JsonStringError::InvalidJson)?;
                let codepoint = if (0xd800..=0xdbff).contains(&first) {
                    let unicode = input.checked_add(1).ok_or(JsonStringError::InvalidJson)?;
                    if source.get(input) != Some(&b'\\') || source.get(unicode) != Some(&b'u') {
                        return Err(JsonStringError::InvalidJson);
                    }
                    let low_start = input.checked_add(2).ok_or(JsonStringError::InvalidJson)?;
                    let second = decode_hex_quad(source, low_start, end)?;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return Err(JsonStringError::InvalidJson);
                    }
                    input = low_start
                        .checked_add(4)
                        .ok_or(JsonStringError::InvalidJson)?;
                    let high = u32::from(first)
                        .checked_sub(0xd800)
                        .and_then(|value| value.checked_shl(10))
                        .ok_or(JsonStringError::InvalidJson)?;
                    let low = u32::from(second)
                        .checked_sub(0xdc00)
                        .ok_or(JsonStringError::InvalidJson)?;
                    0x1_0000_u32
                        .checked_add(high)
                        .and_then(|value| value.checked_add(low))
                        .ok_or(JsonStringError::InvalidJson)?
                } else if (0xdc00..=0xdfff).contains(&first) {
                    return Err(JsonStringError::InvalidJson);
                } else {
                    u32::from(first)
                };
                let character = char::from_u32(codepoint).ok_or(JsonStringError::InvalidJson)?;
                let mut encoded = [0_u8; 4];
                for byte in character.encode_utf8(&mut encoded).bytes() {
                    write_decoded_byte(destination, &mut output, byte)?;
                }
            }
            _ => return Err(JsonStringError::InvalidJson),
        }
    }
    core::str::from_utf8(
        destination
            .get(..output)
            .ok_or(JsonStringError::InvalidJson)?,
    )
    .map_err(|_error| JsonStringError::InvalidJson)?;
    Ok(output)
}

fn decode_hex_quad(source: &[u8], start: usize, end: usize) -> Result<u16, JsonStringError> {
    let stop = start
        .checked_add(4)
        .filter(|stop| *stop <= end)
        .ok_or(JsonStringError::InvalidJson)?;
    let mut value = 0_u16;
    for byte in source
        .get(start..stop)
        .ok_or(JsonStringError::InvalidJson)?
    {
        let digit = match byte {
            b'0'..=b'9' => byte.checked_sub(b'0').map(u16::from),
            b'a'..=b'f' => byte
                .checked_sub(b'a')
                .map(u16::from)
                .and_then(|value| value.checked_add(10)),
            b'A'..=b'F' => byte
                .checked_sub(b'A')
                .map(u16::from)
                .and_then(|value| value.checked_add(10)),
            _ => return Err(JsonStringError::InvalidJson),
        }
        .ok_or(JsonStringError::InvalidJson)?;
        value = value
            .checked_mul(16)
            .and_then(|value| value.checked_add(digit))
            .ok_or(JsonStringError::InvalidJson)?;
    }
    Ok(value)
}

fn write_decoded_byte(
    destination: &mut [u8],
    output: &mut usize,
    byte: u8,
) -> Result<(), JsonStringError> {
    *destination
        .get_mut(*output)
        .ok_or(JsonStringError::TooLong)? = byte;
    *output = output.checked_add(1).ok_or(JsonStringError::InvalidJson)?;
    Ok(())
}

struct SliceWriter<'a> {
    destination: &'a mut [u8],
    written: usize,
}

impl<'a> SliceWriter<'a> {
    const fn new(destination: &'a mut [u8]) -> Self {
        Self {
            destination,
            written: 0,
        }
    }
}

impl fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.written.checked_add(value.len()).ok_or(fmt::Error)?;
        self.destination
            .get_mut(self.written..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(value.as_bytes());
        self.written = end;
        Ok(())
    }
}

async fn read_bounded(
    filesystem: &FileSystem,
    path: &str,
    output: &mut [u8; READ_CONTENT_CAPACITY],
) -> Result<usize, FileRpcError> {
    let mut file = filesystem.filesystem.open(path).await.map_err(map_fs)?;
    let mut filled = 0_usize;
    loop {
        let remaining = output.get_mut(filled..).ok_or(FileRpcError::Io)?;
        if remaining.is_empty() {
            let mut extra = [0_u8; 1];
            let read = file.read(&mut extra).await.map_err(map_fs)?;
            return if read == 0 {
                Ok(filled)
            } else {
                Err(FileRpcError::TooLarge)
            };
        }
        let read = file.read(remaining).await.map_err(map_fs)?;
        if read == 0 {
            return Ok(filled);
        }
        filled = filled.checked_add(read).ok_or(FileRpcError::Io)?;
    }
}

/// Builds the JSON handler for [`FileRead`].
pub(crate) fn read_handler(filesystem: Rc<FileSystem>) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let filesystem = Rc::clone(&filesystem);
        async move {
            let request = request.deserialize::<ReadRequest<'_>>()?;
            let mut request_scratch = [0_u8; REQUEST_CAPACITY];
            let path_length = match decode_json_string(request.path, &mut request_scratch) {
                Ok(length) => length,
                Err(JsonStringError::InvalidJson) => return Err(RpcError::InvalidJson),
                Err(JsonStringError::TooLong) => return Err(RpcError::InvalidFrameState),
            };
            let path = core::str::from_utf8(
                request_scratch
                    .get(..path_length)
                    .ok_or(RpcError::InvalidFrameState)?,
            )
            .map_err(|_error| RpcError::InvalidFrameState)?;
            if let Err(error) = validate_path(path) {
                return response.write(&ErrorResponse(error)).await;
            }

            let mut bytes = [0_u8; READ_CONTENT_CAPACITY];
            match read_bounded(&filesystem, path, &mut bytes).await {
                Ok(length) => match core::str::from_utf8(
                    bytes.get(..length).ok_or(RpcError::InvalidFrameState)?,
                ) {
                    Ok(content) => {
                        let payload = ReadResponse { content };
                        if payload.encoded_len()? > FileRead::MAX_RESPONSE_BYTES {
                            response.write(&ErrorResponse(FileRpcError::TooLarge)).await
                        } else {
                            response.write(&payload).await
                        }
                    }
                    Err(_error) => {
                        response
                            .write(&ErrorResponse(FileRpcError::InvalidUtf8))
                            .await
                    }
                },
                Err(error) => response.write(&ErrorResponse(error)).await,
            }
        }
    }
}

/// Builds the JSON handler for [`FileWrite`].
pub(crate) fn write_handler(filesystem: Rc<FileSystem>) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let filesystem = Rc::clone(&filesystem);
        async move {
            let request = request.deserialize::<WriteRequest<'_>>()?;
            let mut request_scratch = [0_u8; REQUEST_CAPACITY];
            let path_length = match decode_json_string(request.path, &mut request_scratch) {
                Ok(length) => length,
                Err(JsonStringError::InvalidJson) => return Err(RpcError::InvalidJson),
                Err(JsonStringError::TooLong) => return Err(RpcError::InvalidFrameState),
            };
            let content_destination = request_scratch
                .get_mut(path_length..)
                .ok_or(RpcError::InvalidFrameState)?;
            let content_length = match decode_json_string(request.content, content_destination) {
                Ok(length) => length,
                Err(JsonStringError::InvalidJson) => return Err(RpcError::InvalidJson),
                Err(JsonStringError::TooLong) => return Err(RpcError::InvalidFrameState),
            };
            let content_end = path_length
                .checked_add(content_length)
                .ok_or(RpcError::InvalidFrameState)?;
            let path = core::str::from_utf8(
                request_scratch
                    .get(..path_length)
                    .ok_or(RpcError::InvalidFrameState)?,
            )
            .map_err(|_error| RpcError::InvalidFrameState)?;
            if let Err(error) = validate_path(path) {
                return response.write(&ErrorResponse(error)).await;
            }
            let content = request_scratch
                .get(path_length..content_end)
                .ok_or(RpcError::InvalidFrameState)?;
            match filesystem.write(path, content).await {
                Ok(()) => response.write("{}").await,
                Err(error) => response.write(&ErrorResponse(map_fs(error))).await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::{boxed::Box, format, rc::Rc, string::String, vec};

    use barracuda_event_router::{
        JsonRpcSchema, RpcAddress, RpcError, RpcLaneStorage, RpcRegistry,
    };
    use barracuda_vfs::{MountOptions, Vfs};
    use barracuda_vfs_memfs::MemFs;
    use futures_lite::future::block_on;

    use super::{FileRead, FileWrite, READ_CONTENT_CAPACITY, read_handler, write_handler};
    use crate::FileSystem;

    const FRAME_SIZE: usize = 512;

    async fn filesystem() -> Rc<FileSystem> {
        let mut vfs = Vfs::new();
        vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
            .await
            .expect("mount memory filesystem");
        Rc::new(FileSystem::new(
            vfs.scoped("/plugins/file").expect("scope filesystem"),
        ))
    }

    fn registry(filesystem: Rc<FileSystem>) -> RpcRegistry<2, FRAME_SIZE, 2> {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, FRAME_SIZE, 2>::new()));
        let registry = RpcRegistry::new(lanes);
        let _read = registry
            .register_json::<FileRead, _>("*", read_handler(Rc::clone(&filesystem)))
            .expect("register file.read");
        let _write = registry
            .register_json::<FileWrite, _>("*", write_handler(filesystem))
            .expect("register file.write");
        registry
    }

    fn call(registry: &RpcRegistry<2, FRAME_SIZE, 2>, address: &str, request: &str) -> String {
        let address = RpcAddress::try_from(address).expect("valid address");
        let call = registry
            .client()
            .call_json(&address, request)
            .expect("start JSON call");
        block_on(call)
            .expect("complete JSON call")
            .as_str()
            .expect("response JSON")
            .into()
    }

    #[test]
    fn schemas_publish_exact_public_contracts() {
        assert_eq!(FileRead::ADDRESS, "file.read");
        assert_eq!(FileRead::MAX_REQUEST_BYTES, 512);
        assert_eq!(FileRead::MAX_RESPONSE_BYTES, 512);
        assert!(FileRead::REQUEST_SCHEMA.as_str().contains(r#""path""#));
        assert!(FileRead::RESPONSE_SCHEMA.as_str().contains(r#""content""#));
        assert!(!FileRead::REQUEST_SCHEMA.as_str().contains("maxLength"));
        assert!(FileRead::RESPONSE_SCHEMA.as_str().contains("498"));

        assert_eq!(FileWrite::ADDRESS, "file.write");
        assert_eq!(FileWrite::MAX_REQUEST_BYTES, 512);
        assert_eq!(FileWrite::MAX_RESPONSE_BYTES, 29);
        assert!(FileWrite::REQUEST_SCHEMA.as_str().contains(r#""content""#));
        assert!(!FileWrite::REQUEST_SCHEMA.as_str().contains("maxLength"));
        for error in ["invalid_request", "not_found", "permission_denied", "io"] {
            assert!(FileRead::RESPONSE_SCHEMA.as_str().contains(error));
            assert!(FileWrite::RESPONSE_SCHEMA.as_str().contains(error));
        }
        assert!(FileRead::RESPONSE_SCHEMA.as_str().contains("invalid_utf8"));
        assert!(FileRead::RESPONSE_SCHEMA.as_str().contains("too_large"));
        assert!(!FileWrite::RESPONSE_SCHEMA.as_str().contains("invalid_utf8"));
        assert!(!FileWrite::RESPONSE_SCHEMA.as_str().contains("too_large"));
    }

    #[test]
    fn writes_and_reads_escaped_utf8_content() {
        block_on(async {
            let filesystem = filesystem().await;
            let registry = registry(Rc::clone(&filesystem));

            assert_eq!(
                call(
                    &registry,
                    "file.write",
                    r#"{"path":"text/\u4f60\u597d","content":"line 1\n\"你好\"\\end"}"#,
                ),
                "{}"
            );
            assert_eq!(
                filesystem.read("text/你好").await.expect("stored text"),
                "line 1\n\"你好\"\\end".as_bytes()
            );
            assert_eq!(
                call(&registry, "file.read", r#"{"path":"text/\u4f60\u597d"}"#),
                r#"{"content":"line 1\n\"你好\"\\end"}"#
            );
        });
    }

    #[test]
    fn returns_stable_business_errors() {
        block_on(async {
            let filesystem = filesystem().await;
            let registry = registry(Rc::clone(&filesystem));

            assert_eq!(
                call(&registry, "file.read", r#"{"path":"missing"}"#),
                r#"{"error":"not_found"}"#
            );
            filesystem
                .write("invalid", &[0xff])
                .await
                .expect("store invalid UTF-8");
            assert_eq!(
                call(&registry, "file.read", r#"{"path":"invalid"}"#),
                r#"{"error":"invalid_utf8"}"#
            );

            filesystem
                .write("large", &vec![b'A'; READ_CONTENT_CAPACITY + 1])
                .await
                .expect("store oversized file");
            assert_eq!(
                call(&registry, "file.read", r#"{"path":"large"}"#),
                r#"{"error":"too_large"}"#
            );
        });
    }

    #[test]
    fn request_fields_share_the_lane_budget() {
        block_on(async {
            let filesystem = filesystem().await;
            let registry = registry(Rc::clone(&filesystem));

            let content = "A".repeat(400);
            let write = format!(r#"{{"path":"large","content":"{content}"}}"#);
            assert_eq!(call(&registry, "file.write", &write), "{}");
            assert_eq!(
                filesystem.read("large").await.expect("stored large text"),
                content.as_bytes()
            );

            let path = "p".repeat(300);
            let write = format!(r#"{{"path":"{path}","content":"ok"}}"#);
            assert_eq!(call(&registry, "file.write", &write), "{}");
            assert_eq!(
                call(&registry, "file.read", &format!(r#"{{"path":"{path}"}}"#)),
                r#"{"content":"ok"}"#
            );
        });
    }

    #[test]
    fn maximum_file_size_fits_the_declared_response_bound() {
        block_on(async {
            let filesystem = filesystem().await;
            filesystem
                .write("max", &vec![b'A'; READ_CONTENT_CAPACITY])
                .await
                .expect("store maximum readable text");
            let registry = registry(filesystem);

            let response = call(&registry, "file.read", r#"{"path":"max"}"#);
            assert!(response.len() <= FileRead::MAX_RESPONSE_BYTES);
            assert_eq!(
                response,
                format!(r#"{{"content":"{}"}}"#, "A".repeat(READ_CONTENT_CAPACITY))
            );
        });
    }

    #[test]
    fn escaped_read_response_must_still_fit_the_lane() {
        block_on(async {
            let filesystem = filesystem().await;
            filesystem
                .write("escaped", &vec![b'"'; 300])
                .await
                .expect("store escape-heavy text");
            let registry = registry(filesystem);

            assert_eq!(
                call(&registry, "file.read", r#"{"path":"escaped"}"#),
                r#"{"error":"too_large"}"#
            );
        });
    }

    #[test]
    fn oversized_requests_are_transport_errors() {
        block_on(async {
            let registry = registry(filesystem().await);
            let address = RpcAddress::try_from(FileWrite::ADDRESS).expect("valid address");
            let oversized = format!(r#"{{"path":"file","content":"{}"}}"#, "x".repeat(512));
            let error = registry
                .client()
                .call_json(&address, &oversized)
                .err()
                .expect("request exceeds method bound");
            assert!(matches!(error, RpcError::FrameTooLarge { .. }));
        });
    }

    #[test]
    fn schema_mismatches_are_transport_errors() {
        block_on(async {
            let registry = registry(filesystem().await);
            let address = RpcAddress::try_from(FileRead::ADDRESS).expect("valid address");
            let error = registry
                .client()
                .call_json(&address, "[]")
                .expect("start JSON call")
                .await
                .expect_err("request shape must be rejected");
            assert!(matches!(
                error,
                RpcError::JsonRequestSchema {
                    address: FileRead::ADDRESS,
                    ..
                }
            ));
        });
    }

    #[test]
    fn both_methods_are_globally_visible() {
        block_on(async {
            let registry = registry(filesystem().await);
            let expected = [
                RpcAddress::try_from("file.read").expect("valid address"),
                RpcAddress::try_from("file.write").expect("valid address"),
            ];
            assert_eq!(
                registry
                    .client()
                    .rpcs_by_visibility("*")
                    .expect("discover public RPCs"),
                expected
            );
            assert_eq!(
                registry
                    .client()
                    .rpcs_by_visibility("agent")
                    .expect("discover Agent-visible RPCs"),
                expected
            );
        });
    }
}

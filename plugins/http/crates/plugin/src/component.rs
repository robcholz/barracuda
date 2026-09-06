use alloc::{boxed::Box, rc::Rc};
extern crate alloc;

use core::{
    cell::RefCell,
    fmt::{self, Write as _},
};

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, JsonHandler, JsonPayload, JsonRef, JsonRpcSchema,
    JsonSchema, JsonWriter, RegisterContext, RpcError, RunContext, UnregisterContext, json_schema,
};
use http_client::{
    ClientFactory,
    embedded_nal_async::{Dns, TcpConnect},
    reqwless::request::{Method, RequestBuilder as _},
};
use serde::{Deserialize, Deserializer, de};
use serde_json::value::RawValue;

const REQUEST_CAPACITY: usize = 512;
const RESPONSE_CAPACITY: usize = 512;
const SUCCESS_RESPONSE_OVERHEAD: usize = 24;
const RESPONSE_BODY_CAPACITY: usize = RESPONSE_CAPACITY - SUCCESS_RESPONSE_OVERHEAD;
const MIN_ENCODED_HEADER_BYTES: usize = 23;
const HEADER_INDEX_CAPACITY: usize = REQUEST_CAPACITY / MIN_ENCODED_HEADER_BYTES;
const HEADER_BUFFER_SIZE: usize = 16 * 1024;
const WORKSPACE_CAPACITY: usize = 2;

/// JSON contract for one buffered outbound HTTP request.
struct Request;

impl JsonRpcSchema for Request {
    const ADDRESS: &'static str = "http.request";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("request", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("request", response);
    const MAX_REQUEST_BYTES: usize = REQUEST_CAPACITY;
    const MAX_RESPONSE_BYTES: usize = RESPONSE_CAPACITY;
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
}

impl From<HttpMethod> for Method {
    fn from(method: HttpMethod) -> Self {
        match method {
            HttpMethod::Get => Self::GET,
            HttpMethod::Post => Self::POST,
            HttpMethod::Put => Self::PUT,
            HttpMethod::Patch => Self::PATCH,
            HttpMethod::Delete => Self::DELETE,
            HttpMethod::Head => Self::HEAD,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHeader<'a> {
    #[serde(borrow)]
    name: &'a RawValue,
    #[serde(borrow)]
    value: &'a RawValue,
}

#[derive(Clone, Copy, Debug)]
struct RawHeaders<'a> {
    entries: [Option<RawHeader<'a>>; HEADER_INDEX_CAPACITY],
    len: usize,
}

impl Default for RawHeaders<'_> {
    fn default() -> Self {
        Self {
            entries: [None; HEADER_INDEX_CAPACITY],
            len: 0,
        }
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for RawHeaders<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HeaderVisitor<'a>(core::marker::PhantomData<&'a ()>);

        impl<'de: 'a, 'a> de::Visitor<'de> for HeaderVisitor<'a> {
            type Value = RawHeaders<'a>;

            fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                formatter.write_str("HTTP headers fitting the request lane")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let mut headers = RawHeaders::default();
                while let Some(header) = sequence.next_element::<RawHeader<'a>>()? {
                    let slot = headers
                        .entries
                        .get_mut(headers.len)
                        .ok_or_else(|| de::Error::custom("headers exceed request lane"))?;
                    *slot = Some(header);
                    headers.len = headers
                        .len
                        .checked_add(1)
                        .ok_or_else(|| de::Error::custom("headers exceed request lane"))?;
                }
                Ok(headers)
            }
        }

        deserializer.deserialize_seq(HeaderVisitor(core::marker::PhantomData))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequestDocument<'a> {
    method: HttpMethod,
    #[serde(borrow)]
    url: &'a RawValue,
    #[serde(default, borrow)]
    headers: RawHeaders<'a>,
    #[serde(default, borrow)]
    body: Option<&'a RawValue>,
}

#[derive(Clone, Copy, Debug, Default)]
struct TextRange {
    start: u16,
    len: u16,
}

impl TextRange {
    fn as_str(self, scratch: &RequestScratch) -> Result<&str, RpcError> {
        let start = usize::from(self.start);
        let end = start
            .checked_add(usize::from(self.len))
            .ok_or(RpcError::InvalidFrameState)?;
        core::str::from_utf8(
            scratch
                .bytes
                .get(start..end)
                .ok_or(RpcError::InvalidFrameState)?,
        )
        .map_err(|_error| RpcError::InvalidFrameState)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct HeaderRange {
    name: TextRange,
    value: TextRange,
}

struct RequestScratch {
    bytes: [u8; REQUEST_CAPACITY],
    len: usize,
}

impl RequestScratch {
    const fn new() -> Self {
        Self {
            bytes: [0; REQUEST_CAPACITY],
            len: 0,
        }
    }

    fn decode_string(&mut self, raw: &RawValue) -> Result<TextRange, RpcError> {
        let source = raw.get().as_bytes();
        let end = source
            .len()
            .checked_sub(1)
            .filter(|end| source.first() == Some(&b'"') && source.get(*end) == Some(&b'"'))
            .ok_or(RpcError::InvalidJson)?;
        let start = self.len;
        let mut input = 1_usize;
        while input < end {
            let byte = *source.get(input).ok_or(RpcError::InvalidJson)?;
            if byte != b'\\' {
                self.push(byte)?;
                input = input.checked_add(1).ok_or(RpcError::InvalidJson)?;
                continue;
            }

            input = input.checked_add(1).ok_or(RpcError::InvalidJson)?;
            let escape = *source.get(input).ok_or(RpcError::InvalidJson)?;
            input = input.checked_add(1).ok_or(RpcError::InvalidJson)?;
            match escape {
                b'"' | b'\\' | b'/' => self.push(escape)?,
                b'b' => self.push(0x08)?,
                b'f' => self.push(0x0c)?,
                b'n' => self.push(b'\n')?,
                b'r' => self.push(b'\r')?,
                b't' => self.push(b'\t')?,
                b'u' => {
                    let first = decode_hex_quad(source, input, end)?;
                    input = input.checked_add(4).ok_or(RpcError::InvalidJson)?;
                    let codepoint = if (0xd800..=0xdbff).contains(&first) {
                        let unicode = input.checked_add(1).ok_or(RpcError::InvalidJson)?;
                        if source.get(input) != Some(&b'\\') || source.get(unicode) != Some(&b'u') {
                            return Err(RpcError::InvalidJson);
                        }
                        let low_start = input.checked_add(2).ok_or(RpcError::InvalidJson)?;
                        let second = decode_hex_quad(source, low_start, end)?;
                        if !(0xdc00..=0xdfff).contains(&second) {
                            return Err(RpcError::InvalidJson);
                        }
                        input = low_start.checked_add(4).ok_or(RpcError::InvalidJson)?;
                        let high = u32::from(first)
                            .checked_sub(0xd800)
                            .and_then(|value| value.checked_shl(10))
                            .ok_or(RpcError::InvalidJson)?;
                        let low = u32::from(second)
                            .checked_sub(0xdc00)
                            .ok_or(RpcError::InvalidJson)?;
                        0x1_0000_u32
                            .checked_add(high)
                            .and_then(|value| value.checked_add(low))
                            .ok_or(RpcError::InvalidJson)?
                    } else if (0xdc00..=0xdfff).contains(&first) {
                        return Err(RpcError::InvalidJson);
                    } else {
                        u32::from(first)
                    };
                    let character = char::from_u32(codepoint).ok_or(RpcError::InvalidJson)?;
                    let mut encoded = [0_u8; 4];
                    for byte in character.encode_utf8(&mut encoded).bytes() {
                        self.push(byte)?;
                    }
                }
                _ => return Err(RpcError::InvalidJson),
            }
        }
        let length = self
            .len
            .checked_sub(start)
            .ok_or(RpcError::InvalidFrameState)?;
        core::str::from_utf8(
            self.bytes
                .get(start..self.len)
                .ok_or(RpcError::InvalidFrameState)?,
        )
        .map_err(|_error| RpcError::InvalidJson)?;
        Ok(TextRange {
            start: u16::try_from(start).map_err(|_error| RpcError::InvalidFrameState)?,
            len: u16::try_from(length).map_err(|_error| RpcError::InvalidFrameState)?,
        })
    }

    fn push(&mut self, byte: u8) -> Result<(), RpcError> {
        *self
            .bytes
            .get_mut(self.len)
            .ok_or(RpcError::InvalidFrameState)? = byte;
        self.len = self.len.checked_add(1).ok_or(RpcError::InvalidFrameState)?;
        Ok(())
    }
}

fn decode_hex_quad(source: &[u8], start: usize, end: usize) -> Result<u16, RpcError> {
    let stop = start
        .checked_add(4)
        .filter(|stop| *stop <= end)
        .ok_or(RpcError::InvalidJson)?;
    let mut value = 0_u16;
    for byte in source.get(start..stop).ok_or(RpcError::InvalidJson)? {
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
            _ => return Err(RpcError::InvalidJson),
        }
        .ok_or(RpcError::InvalidJson)?;
        value = value
            .checked_mul(16)
            .and_then(|value| value.checked_add(digit))
            .ok_or(RpcError::InvalidJson)?;
    }
    Ok(value)
}

struct DecodedRequest {
    method: HttpMethod,
    scratch: RequestScratch,
    url: TextRange,
    headers: [HeaderRange; HEADER_INDEX_CAPACITY],
    header_count: usize,
    body: TextRange,
}

impl DecodedRequest {
    fn decode(raw: &RawRequestDocument<'_>) -> Result<Self, RpcError> {
        let mut scratch = RequestScratch::new();
        let url = scratch.decode_string(raw.url)?;
        let mut headers = [HeaderRange::default(); HEADER_INDEX_CAPACITY];
        let mut header_count = 0_usize;
        for (output, input) in headers.iter_mut().zip(raw.headers.entries.iter().flatten()) {
            *output = HeaderRange {
                name: scratch.decode_string(input.name)?,
                value: scratch.decode_string(input.value)?,
            };
            header_count = header_count
                .checked_add(1)
                .ok_or(RpcError::InvalidFrameState)?;
        }
        if header_count != raw.headers.len {
            return Err(RpcError::InvalidFrameState);
        }
        let body = match raw.body {
            Some(body) => scratch.decode_string(body)?,
            None => TextRange::default(),
        };
        Ok(Self {
            method: raw.method,
            scratch,
            url,
            headers,
            header_count,
            body,
        })
    }

    fn view(&self) -> Result<RequestView<'_>, RpcError> {
        let mut headers = [("", ""); HEADER_INDEX_CAPACITY];
        let ranges = self
            .headers
            .get(..self.header_count)
            .ok_or(RpcError::InvalidFrameState)?;
        for (output, range) in headers.iter_mut().zip(ranges) {
            *output = (
                range.name.as_str(&self.scratch)?,
                range.value.as_str(&self.scratch)?,
            );
        }
        Ok(RequestView {
            method: self.method,
            url: self.url.as_str(&self.scratch)?,
            headers,
            header_count: self.header_count,
            body: self.body.as_str(&self.scratch)?,
        })
    }
}

struct RequestView<'a> {
    method: HttpMethod,
    url: &'a str,
    headers: [(&'a str, &'a str); HEADER_INDEX_CAPACITY],
    header_count: usize,
    body: &'a str,
}

impl RequestView<'_> {
    fn validate(&self, tls: bool) -> Result<(), HttpError> {
        if !self.url.starts_with("http://") && !self.url.starts_with("https://") {
            return Err(HttpError::InvalidUrl);
        }
        if self.url.starts_with("https://") && !tls {
            return Err(HttpError::TlsNotConfigured);
        }
        for (name, value) in self.headers.iter().take(self.header_count) {
            if name.is_empty()
                || name.bytes().any(|byte| byte <= b' ' || byte == b':')
                || value.contains('\r')
                || value.contains('\n')
            {
                return Err(HttpError::InvalidHeader);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HttpError {
    InvalidUrl,
    TlsNotConfigured,
    InvalidHeader,
    Transport,
    InvalidResponseText,
    ResponseTooLarge,
}

struct ErrorResponse(HttpError);

impl JsonPayload for ErrorResponse {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        error_document(self.0).encoded_len()
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        error_document(self.0).write_json(destination)
    }
}

const fn error_document(error: HttpError) -> &'static str {
    match error {
        HttpError::InvalidUrl => r#"{"error":"invalid_url"}"#,
        HttpError::TlsNotConfigured => r#"{"error":"tls_not_configured"}"#,
        HttpError::InvalidHeader => r#"{"error":"invalid_header"}"#,
        HttpError::Transport => r#"{"error":"transport"}"#,
        HttpError::InvalidResponseText => r#"{"error":"invalid_response_text"}"#,
        HttpError::ResponseTooLarge => r#"{"error":"response_too_large"}"#,
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ResponseBody {
    bytes: [u8; RESPONSE_BODY_CAPACITY],
    len: usize,
}

impl ResponseBody {
    const fn new() -> Self {
        Self {
            bytes: [0; RESPONSE_BODY_CAPACITY],
            len: 0,
        }
    }

    fn as_str(&self) -> Result<&str, HttpError> {
        let bytes = self
            .bytes
            .get(..self.len)
            .ok_or(HttpError::InvalidResponseText)?;
        core::str::from_utf8(bytes).map_err(|_error| HttpError::InvalidResponseText)
    }
}

struct HttpWorkspace {
    header_buffer: [u8; HEADER_BUFFER_SIZE],
    response_body: ResponseBody,
}

impl HttpWorkspace {
    const fn new() -> Self {
        Self {
            header_buffer: [0; HEADER_BUFFER_SIZE],
            response_body: ResponseBody::new(),
        }
    }
}

struct WorkspacePool {
    slots: RefCell<[Option<Box<HttpWorkspace>>; WORKSPACE_CAPACITY]>,
}

impl WorkspacePool {
    fn new() -> Self {
        Self {
            slots: RefCell::new(core::array::from_fn(|_| {
                Some(Box::new(HttpWorkspace::new()))
            })),
        }
    }

    fn acquire(self: &Rc<Self>) -> Result<WorkspaceLease, RpcError> {
        let mut slots = self.slots.borrow_mut();
        let workspace =
            slots
                .iter_mut()
                .find_map(Option::take)
                .ok_or(RpcError::ResourceExhausted {
                    resource: "HTTP request workspace",
                    limit: WORKSPACE_CAPACITY,
                })?;
        drop(slots);
        Ok(WorkspaceLease {
            pool: Rc::clone(self),
            workspace: Some(workspace),
        })
    }
}

struct WorkspaceLease {
    pool: Rc<WorkspacePool>,
    workspace: Option<Box<HttpWorkspace>>,
}

impl WorkspaceLease {
    fn get_mut(&mut self) -> Result<&mut HttpWorkspace, RpcError> {
        self.workspace
            .as_deref_mut()
            .ok_or(RpcError::InvalidFrameState)
    }
}

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        let Some(workspace) = self.workspace.take() else {
            return;
        };
        let mut slots = self.pool.slots.borrow_mut();
        if let Some(slot) = slots.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(workspace);
        }
    }
}

struct SuccessResponse<'a> {
    status: u16,
    body: &'a str,
}

impl JsonPayload for SuccessResponse<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        if !(100..=599).contains(&self.status) {
            return Err(RpcError::InvalidFrameState);
        }
        let mut length = SUCCESS_RESPONSE_OVERHEAD;
        for character in self.body.chars() {
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
        write!(writer, "{{\"status\":{},\"body\":\"", self.status)
            .map_err(|_error| RpcError::FrameTooLarge { size, capacity })?;
        write_json_string_body(&mut writer, self.body)
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

fn request_handler<T, D>(
    clients: ClientFactory<'static, T, D>,
    workspaces: Rc<WorkspacePool>,
) -> impl JsonHandler
where
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    move |_context, request: JsonRef, response: JsonWriter| {
        let clients = clients.clone();
        let workspaces = Rc::clone(&workspaces);
        async move {
            let request = request.deserialize::<RawRequestDocument<'_>>()?;
            let request = DecodedRequest::decode(&request)?;
            let request = request.view()?;
            let mut workspace = workspaces.acquire()?;
            match execute(&clients, &request, workspace.get_mut()?).await {
                Ok(status) => {
                    let body = workspace
                        .get_mut()?
                        .response_body
                        .as_str()
                        .map_err(|_error| RpcError::InvalidFrameState)?;
                    let document = SuccessResponse { status, body };
                    if document.encoded_len()? <= Request::MAX_RESPONSE_BYTES {
                        response.write(&document).await
                    } else {
                        response
                            .write(&ErrorResponse(HttpError::ResponseTooLarge))
                            .await
                    }
                }
                Err(error) => response.write(&ErrorResponse(error)).await,
            }
        }
    }
}

async fn execute<T: TcpConnect, D: Dns>(
    clients: &ClientFactory<'_, T, D>,
    input: &RequestView<'_>,
    workspace: &mut HttpWorkspace,
) -> Result<u16, HttpError> {
    let (mut client, tls) = clients.create();
    input.validate(tls)?;
    let headers = input
        .headers
        .get(..input.header_count)
        .ok_or(HttpError::InvalidHeader)?;
    let request = client
        .request(input.method.into(), input.url)
        .await
        .map_err(|_error| HttpError::Transport)?;
    let mut request = request.headers(headers).body(input.body.as_bytes());
    let response = request
        .send(&mut workspace.header_buffer)
        .await
        .map_err(|_error| HttpError::Transport)?;
    let status = response.status.0;
    if !(100..=599).contains(&status) {
        return Err(HttpError::Transport);
    }
    let mut reader = response.body().reader();
    collect_response_body(&mut reader, &mut workspace.response_body).await?;
    workspace.response_body.as_str()?;
    Ok(status)
}

async fn collect_response_body<R: embedded_io_async::Read>(
    reader: &mut R,
    body: &mut ResponseBody,
) -> Result<(), HttpError> {
    body.len = 0;
    loop {
        let destination = body
            .bytes
            .get_mut(body.len..)
            .ok_or(HttpError::ResponseTooLarge)?;
        if destination.is_empty() {
            let mut extra = [0; 1];
            let more = embedded_io_async::Read::read(reader, &mut extra)
                .await
                .map_err(|_error| HttpError::Transport)?;
            if more == 0 {
                break;
            }
            return Err(HttpError::ResponseTooLarge);
        }
        let read = embedded_io_async::Read::read(reader, destination)
            .await
            .map_err(|_error| HttpError::Transport)?;
        if read == 0 {
            break;
        }
        body.len = body
            .len
            .checked_add(read)
            .ok_or(HttpError::ResponseTooLarge)?;
    }
    Ok(())
}

/// Event Router Component serving the JSON `http.request` RPC.
pub struct HttpComponent<T: 'static = http_client::Tcp, D: 'static = http_client::Resolver> {
    clients: ClientFactory<'static, T, D>,
    workspaces: Rc<WorkspacePool>,
}

impl<T, D> HttpComponent<T, D> {
    /// Creates a Component from System's shared HTTP client factory.
    #[must_use]
    pub fn new(clients: ClientFactory<'static, T, D>) -> Self {
        Self {
            clients,
            workspaces: Rc::new(WorkspacePool::new()),
        }
    }
}

impl<T: TcpConnect + 'static, D: Dns + 'static, const M: usize> Component<M>
    for HttpComponent<T, D>
{
    fn name(&self) -> &'static str {
        "http"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_json::<Request, _>(
            "*",
            request_handler(self.clients.clone(), Rc::clone(&self.workspaces)),
        )
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(core::future::pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(clippy::indexing_slicing)]
    #![allow(clippy::panic)]

    use core::{
        future::Future,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        pin::pin,
        task::{Context, Poll, Waker},
    };

    use barracuda_event_router::{RpcAddress, RpcLaneStorage, RpcRegistry};
    use embedded_io::{ErrorKind, ErrorType};
    use futures_lite::future::block_on;
    use http_client::embedded_nal_async::AddrType;
    use serde_json::Value;

    use super::*;

    struct ScriptedNetwork {
        response: &'static [u8],
    }

    struct ScriptedConnection {
        response: &'static [u8],
        offset: usize,
    }

    impl ErrorType for ScriptedConnection {
        type Error = ErrorKind;
    }

    impl embedded_io_async::Read for ScriptedConnection {
        async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
            let remaining = self.response.get(self.offset..).ok_or(ErrorKind::Other)?;
            let count = remaining.len().min(buffer.len());
            let source = remaining.get(..count).ok_or(ErrorKind::Other)?;
            let destination = buffer.get_mut(..count).ok_or(ErrorKind::Other)?;
            destination.copy_from_slice(source);
            self.offset = self.offset.checked_add(count).ok_or(ErrorKind::Other)?;
            Ok(count)
        }
    }

    impl embedded_io_async::Write for ScriptedConnection {
        async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
            Ok(buffer.len())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl TcpConnect for ScriptedNetwork {
        type Error = ErrorKind;
        type Connection<'a>
            = ScriptedConnection
        where
            Self: 'a;

        async fn connect<'a>(
            &'a self,
            _remote: SocketAddr,
        ) -> Result<Self::Connection<'a>, Self::Error> {
            Ok(ScriptedConnection {
                response: self.response,
                offset: 0,
            })
        }
    }

    impl Dns for ScriptedNetwork {
        type Error = ErrorKind;

        async fn get_host_by_name(
            &self,
            _host: &str,
            _addr_type: AddrType,
        ) -> Result<IpAddr, Self::Error> {
            Ok(IpAddr::V4(Ipv4Addr::LOCALHOST))
        }

        async fn get_host_by_address(
            &self,
            _addr: IpAddr,
            result: &mut [u8],
        ) -> Result<usize, Self::Error> {
            let name = b"localhost";
            let destination = result.get_mut(..name.len()).ok_or(ErrorKind::Other)?;
            destination.copy_from_slice(name);
            Ok(name.len())
        }
    }

    fn registry(response: &'static [u8]) -> RpcRegistry<2, 512, 2> {
        let network: &'static ScriptedNetwork = Box::leak(Box::new(ScriptedNetwork { response }));
        let clients = ClientFactory::from_network(network, network);
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 512, 2>::new()));
        let registry = RpcRegistry::new(lanes);
        let _registration = registry
            .register_json::<Request, _>(
                "*",
                request_handler(clients, Rc::new(WorkspacePool::new())),
            )
            .expect("register http.request");
        registry
    }

    fn call(registry: &RpcRegistry<2, 512, 2>, request: &str) -> Result<Value, RpcError> {
        let address = RpcAddress::try_from(Request::ADDRESS).expect("valid address");
        let response = block_on(
            registry
                .client()
                .call_json(&address, request)
                .expect("start JSON call"),
        )?;
        response.deserialize()
    }

    struct SliceReader<'a> {
        remaining: &'a [u8],
    }

    impl ErrorType for SliceReader<'_> {
        type Error = ErrorKind;
    }

    impl embedded_io_async::Read for SliceReader<'_> {
        async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
            let count = self.remaining.len().min(buffer.len());
            let (source, rest) = self.remaining.split_at(count);
            let destination = buffer.get_mut(..count).ok_or(ErrorKind::Other)?;
            destination.copy_from_slice(source);
            self.remaining = rest;
            Ok(count)
        }
    }

    fn ready<T>(future: impl Future<Output = T>) -> T {
        let mut future = pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("future should complete without external I/O"),
        }
    }

    #[test]
    fn contract_is_public_bounded_json() {
        assert_eq!(Request::ADDRESS, "http.request");
        assert_eq!(Request::MAX_REQUEST_BYTES, 512);
        assert_eq!(Request::MAX_RESPONSE_BYTES, 512);
        assert!(
            Request::REQUEST_SCHEMA
                .as_str()
                .contains("HTTP request method")
        );
        assert!(
            Request::RESPONSE_SCHEMA
                .as_str()
                .contains("response_too_large")
        );
        assert!(!Request::REQUEST_SCHEMA.as_str().contains("maxLength"));
        assert!(!Request::REQUEST_SCHEMA.as_str().contains("maxItems"));

        let registry = registry(b"");
        assert_eq!(
            registry
                .client()
                .rpcs_by_visibility("*")
                .expect("public RPC list"),
            [RpcAddress::try_from("http.request").expect("valid address")]
        );
        assert_eq!(
            registry
                .client()
                .rpcs_by_visibility("agent")
                .expect("Agent-visible RPC list"),
            [RpcAddress::try_from("http.request").expect("valid address")]
        );
    }

    #[test]
    fn request_workspace_pool_has_a_fixed_concurrency_bound() {
        let pool = Rc::new(WorkspacePool::new());
        let first = pool.acquire().expect("first workspace");
        let _second = pool.acquire().expect("second workspace");
        assert!(matches!(
            pool.acquire(),
            Err(RpcError::ResourceExhausted {
                resource: "HTTP request workspace",
                limit: WORKSPACE_CAPACITY,
            })
        ));
        drop(first);
        let _reused = pool.acquire().expect("released workspace is reused");
    }

    #[test]
    fn request_returns_status_and_utf8_body() {
        let registry = registry(b"HTTP/1.1 201 Created\r\nContent-Length: 5\r\n\r\nhello");
        let response = call(
            &registry,
            r#"{"method":"POST","url":"http://example.com/items","headers":[{"name":"Content-Type","value":"text/plain"}],"body":"hello"}"#,
        )
        .expect("successful HTTP JSON call");

        assert_eq!(response["status"], 201);
        assert_eq!(response["body"], "hello");
    }

    #[test]
    fn request_accepts_escaped_json_strings_without_heap_strings() {
        let registry = registry(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let response = call(
            &registry,
            r#"{"method":"POST","url":"http://example.com/\u4f60\u597d","headers":[{"name":"X-Note","value":"line\tvalue"}],"body":"{\"name\":\"example\"}\n"}"#,
        )
        .expect("escaped strings decode into fixed-capacity storage");

        assert_eq!(response["status"], 200);
        assert_eq!(response["body"], "ok");
    }

    #[test]
    fn request_fields_share_the_lane_budget() {
        let registry = registry(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");

        let long_url = alloc::format!("http://example.com/{}", "u".repeat(210));
        let request = alloc::format!(r#"{{"method":"GET","url":"{long_url}"}}"#);
        assert_eq!(
            call(&registry, &request).expect("long URL fits total lane")["status"],
            200
        );

        let request = alloc::format!(
            r#"{{"method":"POST","url":"http://example.com","body":"{}"}}"#,
            "b".repeat(200)
        );
        assert_eq!(
            call(&registry, &request).expect("long body fits total lane")["status"],
            200
        );

        let request = alloc::format!(
            r#"{{"method":"GET","url":"http://example.com","headers":[{{"name":"X-{}","value":"{}"}},{{"name":"Y","value":"2"}},{{"name":"Z","value":"3"}}]}}"#,
            "n".repeat(40),
            "v".repeat(100)
        );
        assert_eq!(
            call(&registry, &request).expect("headers share total lane")["status"],
            200
        );
    }

    #[test]
    fn validation_failures_use_their_contract_boundaries() {
        let registry = registry(b"");
        assert!(matches!(
            call(&registry, r#"{"method":"GET","url":"ftp://example.com"}"#),
            Err(RpcError::JsonRequestSchema {
                address: Request::ADDRESS,
                ..
            })
        ));
        assert_eq!(
            call(
                &registry,
                r#"{"method":"GET","url":"http://example.com","headers":[{"name":"X Bad","value":"1"}]}"#
            )
            .expect("business response")["error"],
            "invalid_header"
        );
    }

    #[test]
    fn upstream_failures_are_business_documents() {
        let transport = registry(b"");
        assert_eq!(
            call(&transport, r#"{"method":"GET","url":"http://example.com"}"#)
                .expect("transport response")["error"],
            "transport"
        );

        let invalid_utf8 = registry(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n\xff");
        assert_eq!(
            call(
                &invalid_utf8,
                r#"{"method":"GET","url":"http://example.com"}"#
            )
            .expect("invalid response text document")["error"],
            "invalid_response_text"
        );
    }

    #[test]
    fn malformed_and_oversized_requests_are_rpc_errors() {
        let registry = registry(b"");
        let address = RpcAddress::try_from(Request::ADDRESS).expect("valid address");
        let error = block_on(
            registry
                .client()
                .call_json(&address, "[]")
                .expect("start malformed call"),
        )
        .expect_err("array is not a request object");
        assert!(matches!(
            error,
            RpcError::JsonRequestSchema {
                address: Request::ADDRESS,
                ..
            }
        ));

        let oversized = alloc::format!(
            "{{\"method\":\"POST\",\"url\":\"http://example.com\",\"body\":\"{}\"}}",
            "x".repeat(512)
        );
        assert!(matches!(
            registry.client().call_json(&address, &oversized),
            Err(RpcError::FrameTooLarge { capacity: 512, .. })
        ));
    }

    #[test]
    fn response_body_boundary_is_explicit() {
        let at_capacity = alloc::vec![b'a'; RESPONSE_BODY_CAPACITY];
        let mut body = ResponseBody::new();
        ready(collect_response_body(
            &mut SliceReader {
                remaining: &at_capacity,
            },
            &mut body,
        ))
        .expect("bounded body");
        assert_eq!(
            body.as_str().expect("UTF-8 body").len(),
            RESPONSE_BODY_CAPACITY
        );
        let document = SuccessResponse {
            status: 200,
            body: body.as_str().expect("UTF-8 body"),
        };
        assert_eq!(document.encoded_len(), Ok(Request::MAX_RESPONSE_BYTES));

        let over_capacity = alloc::vec![b'a'; RESPONSE_BODY_CAPACITY + 1];
        let mut body = ResponseBody::new();
        assert_eq!(
            ready(collect_response_body(
                &mut SliceReader {
                    remaining: &over_capacity,
                },
                &mut body,
            )),
            Err(HttpError::ResponseTooLarge)
        );
    }

    #[test]
    fn escaped_response_that_exceeds_lane_becomes_business_error() {
        let response = alloc::vec![b'"'; RESPONSE_BODY_CAPACITY];
        let mut http =
            alloc::vec::Vec::from(&b"HTTP/1.1 200 OK\r\nContent-Length: 488\r\n\r\n"[..]);
        http.extend_from_slice(&response);
        let registry = registry(Box::leak(http.into_boxed_slice()));

        assert_eq!(
            call(&registry, r#"{"method":"GET","url":"http://example.com"}"#)
                .expect("business response")["error"],
            "response_too_large"
        );
    }
}

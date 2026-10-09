//! HTTP request helpers owned by the IMessage Gateway.
#![no_std]

extern crate alloc;

use alloc::{
    collections::VecDeque,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::cell::{Cell, RefCell};

use barracuda_bulk_memory::BulkBox;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use futures_lite::StreamExt as _;
use gateway::BinaryBody;
use reqwless::request::RequestBuilder as _;

pub use reqwless::request::{Method, RequestBody};

mod poll;
pub use poll::{poll, PollEnd, PollLimits, PollRequest, Polled, Poller};

const HEADER_BUFFER_SIZE: usize = 16 * 1024;
const READ_BUFFER_SIZE: usize = 8 * 1024;

/// Deadline of one [`send`] exchange, matching the `plugins/http` request
/// deadline.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTPS requested without a TLS configuration")]
    TlsNotConfigured,
    /// The exchange missed its [`SEND_TIMEOUT`] deadline.
    #[error("HTTP request timed out")]
    Timeout,
    /// The response body exceeded the limit given to [`send_limited`].
    #[error("HTTP response body is too large")]
    BodyTooLarge,
    #[error(transparent)]
    Reqwless(#[from] reqwless::Error),
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Sends one request on a fresh connection and buffers the whole response.
///
/// The exchange (DNS, connect, TLS handshake, request, and response) must
/// finish within [`SEND_TIMEOUT`], otherwise it is dropped with
/// [`Error::Timeout`], which providers report as a transport error. A body of
/// unknown length is a caller-fed stream: time spent waiting for its next
/// chunk does not count, but connecting, each write, and the response after
/// the last chunk are each bounded by [`SEND_TIMEOUT`].
pub async fn send<T, D, B>(
    http_clients: &http_client::ClientFactory<'_, T, D>,
    method: Method,
    url: &str,
    headers: &[(&str, &str)],
    body: B,
) -> Result<Response, Error>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
    B: RequestBody,
{
    send_within(SEND_TIMEOUT, http_clients, method, url, headers, body).await
}

/// [`send`] that stops reading once the response body exceeds `limit` bytes
/// and fails with [`Error::BodyTooLarge`], so a receive path never buffers
/// more than it can afford.
pub async fn send_limited<T, D, B>(
    http_clients: &http_client::ClientFactory<'_, T, D>,
    method: Method,
    url: &str,
    headers: &[(&str, &str)],
    body: B,
    limit: usize,
) -> Result<Response, Error>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
    B: RequestBody,
{
    send_bounded(
        SEND_TIMEOUT,
        limit,
        http_clients,
        method,
        url,
        headers,
        body,
    )
    .await
}

async fn send_within<T, D, B>(
    timeout: Duration,
    http_clients: &http_client::ClientFactory<'_, T, D>,
    method: Method,
    url: &str,
    headers: &[(&str, &str)],
    body: B,
) -> Result<Response, Error>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
    B: RequestBody,
{
    send_bounded(
        timeout,
        usize::MAX,
        http_clients,
        method,
        url,
        headers,
        body,
    )
    .await
}

async fn send_bounded<T, D, B>(
    timeout: Duration,
    limit: usize,
    http_clients: &http_client::ClientFactory<'_, T, D>,
    method: Method,
    url: &str,
    headers: &[(&str, &str)],
    body: B,
) -> Result<Response, Error>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
    B: RequestBody,
{
    let deadline = Deadline::new(timeout, body.len().is_none());
    let body = PacedBody {
        body,
        deadline: &deadline,
    };
    futures_lite::future::or(
        exchange(http_clients, method, url, headers, body, limit),
        async {
            deadline.expired().await;
            Err(Error::Timeout)
        },
    )
    .await
}

async fn exchange<T, D, B>(
    http_clients: &http_client::ClientFactory<'_, T, D>,
    method: Method,
    url: &str,
    headers: &[(&str, &str)],
    body: B,
    limit: usize,
) -> Result<Response, Error>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
    B: RequestBody,
{
    let (mut client, tls_configured) = http_clients.create();
    if url.starts_with("https://") && !tls_configured {
        return Err(Error::TlsNotConfigured);
    }
    let mut header_buffer = BulkBox::<[u8]>::new_zeroed_slice(HEADER_BUFFER_SIZE);
    let request = client.request(method, url).await?;
    let mut request = request.headers(headers).body(body);
    let response = request.send(&mut header_buffer[..]).await?;
    let status = response.status.0;
    let mut reader = response.body().reader();
    let mut read_buffer = BulkBox::<[u8]>::new_zeroed_slice(READ_BUFFER_SIZE);
    let mut bytes = Vec::new();
    loop {
        let read = embedded_io_async::Read::read(&mut reader, &mut read_buffer)
            .await
            .map_err(|error| reqwless::Error::Network(embedded_io::Error::kind(&error)))?;
        if read == 0 {
            break;
        }
        let chunk = read_buffer.get(..read).ok_or(reqwless::Error::Codec)?;
        if bytes.len().saturating_add(read) > limit {
            return Err(Error::BodyTooLarge);
        }
        bytes.extend_from_slice(chunk);
    }
    Ok(Response {
        status,
        body: bytes,
    })
}

/// Expiry instant of one [`send`] exchange, paused while a streamed body waits
/// for its caller.
struct Deadline {
    /// Instant the exchange expires at, or `None` while paused.
    at: Cell<Option<Instant>>,
    timeout: Duration,
    /// Whether the clock pauses between writes of a streamed body.
    streamed: bool,
    changed: Signal<NoopRawMutex, ()>,
}

impl Deadline {
    fn new(timeout: Duration, streamed: bool) -> Self {
        Self {
            at: Cell::new(Some(Instant::now().saturating_add(timeout))),
            timeout,
            streamed,
            changed: Signal::new(),
        }
    }

    /// Restarts the clock from now for a streamed body.
    fn resume(&self) {
        if self.streamed {
            self.set(Some(Instant::now().saturating_add(self.timeout)));
        }
    }

    /// Stops the clock while a streamed body waits for its next chunk.
    fn pause(&self) {
        if self.streamed {
            self.set(None);
        }
    }

    fn set(&self, at: Option<Instant>) {
        self.at.set(at);
        self.changed.signal(());
    }

    /// Completes once the current expiry instant passes without a change.
    async fn expired(&self) {
        loop {
            match self.at.get() {
                Some(at) if Instant::now() >= at => return,
                Some(at) => futures_lite::future::or(Timer::at(at), self.changed.wait()).await,
                None => self.changed.wait().await,
            }
        }
    }
}

/// Request body that drives the [`Deadline`] around the writes of `body`.
struct PacedBody<'a, B> {
    body: B,
    deadline: &'a Deadline,
}

impl<B: RequestBody> RequestBody for PacedBody<'_, B> {
    fn len(&self) -> Option<usize> {
        self.body.len()
    }

    async fn write<W: embedded_io_async::Write>(&self, writer: &mut W) -> Result<(), W::Error> {
        self.deadline.pause();
        let mut writer = PacedWriter {
            writer,
            deadline: self.deadline,
        };
        let written = self.body.write(&mut writer).await;
        self.deadline.resume();
        written
    }
}

/// Writer that runs the [`Deadline`] only while a write is in flight.
struct PacedWriter<'a, W> {
    writer: &'a mut W,
    deadline: &'a Deadline,
}

impl<W: embedded_io_async::ErrorType> embedded_io_async::ErrorType for PacedWriter<'_, W> {
    type Error = W::Error;
}

impl<W: embedded_io_async::Write> embedded_io_async::Write for PacedWriter<'_, W> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.deadline.resume();
        let written = self.writer.write(buf).await;
        self.deadline.pause();
        written
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.deadline.resume();
        let flushed = self.writer.flush().await;
        self.deadline.pause();
        flushed
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MultipartError {
    #[error("invalid multipart metadata")]
    InvalidMetadata,
    #[error("multipart body is too large")]
    LengthOverflow,
}

enum Segment {
    Bytes(Vec<u8>),
    Stream(gateway::BinaryStream),
}

struct BodyState {
    segments: VecDeque<Segment>,
    failure: Option<String>,
    completed: bool,
}

/// Multipart body that writes directly through reqwless without buffering streams.
#[derive(Clone)]
pub struct MultipartBody {
    state: Rc<RefCell<BodyState>>,
    content_length: Option<usize>,
}

impl MultipartBody {
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        self.state.borrow().failure.clone()
    }
}

impl RequestBody for MultipartBody {
    fn len(&self) -> Option<usize> {
        self.content_length
    }

    async fn write<W: embedded_io_async::Write>(&self, writer: &mut W) -> Result<(), W::Error> {
        if self.state.borrow().completed {
            return Ok(());
        }
        loop {
            let segment = self.state.borrow_mut().segments.pop_front();
            match segment {
                Some(Segment::Bytes(bytes)) => writer.write_all(&bytes).await?,
                Some(Segment::Stream(mut stream)) => {
                    while let Some(chunk) = stream.next().await {
                        match chunk {
                            Ok(bytes) => writer.write_all(&bytes).await?,
                            Err(error) => {
                                let mut state = self.state.borrow_mut();
                                state.failure = Some(error.to_string());
                                state.completed = true;
                                return Ok(());
                            }
                        }
                    }
                }
                None => {
                    self.state.borrow_mut().completed = true;
                    return Ok(());
                }
            }
        }
    }
}

enum Part {
    Text {
        name: String,
        value: String,
    },
    File {
        name: String,
        filename: String,
        content_type: String,
        body: BinaryBody,
    },
}

pub struct Multipart {
    boundary: String,
    parts: Vec<Part>,
}

impl Multipart {
    #[must_use]
    pub fn new(boundary: impl Into<String>) -> Self {
        Self {
            boundary: boundary.into(),
            parts: Vec::new(),
        }
    }

    #[must_use]
    pub fn text(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.parts.push(Part::Text {
            name: name.into(),
            value: value.into(),
        });
        self
    }

    #[must_use]
    pub fn file(
        mut self,
        name: impl Into<String>,
        filename: impl Into<String>,
        content_type: impl Into<String>,
        body: BinaryBody,
    ) -> Self {
        self.parts.push(Part::File {
            name: name.into(),
            filename: filename.into(),
            content_type: content_type.into(),
            body,
        });
        self
    }

    pub fn finish(self) -> Result<(String, MultipartBody), MultipartError> {
        validate_boundary(&self.boundary)?;
        let content_type = format!("multipart/form-data; boundary={}", self.boundary);
        let mut segments = VecDeque::new();
        let mut length = Some(0usize);
        for part in self.parts {
            match part {
                Part::Text { name, value } => {
                    validate_quoted(&name)?;
                    push_bytes(
                        &mut segments,
                        &mut length,
                        format!(
                            "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n",
                            self.boundary, name, value
                        )
                        .into_bytes(),
                    )?;
                }
                Part::File {
                    name,
                    filename,
                    content_type,
                    body,
                } => {
                    validate_quoted(&name)?;
                    validate_quoted(&filename)?;
                    validate_header_value(&content_type)?;
                    push_bytes(
                        &mut segments,
                        &mut length,
                        format!(
                            "--{}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                            self.boundary, name, filename, content_type
                        )
                        .into_bytes(),
                    )?;
                    match body {
                        BinaryBody::Bytes(bytes) => push_bytes(&mut segments, &mut length, bytes)?,
                        BinaryBody::Stream(stream) => {
                            length = None;
                            segments.push_back(Segment::Stream(stream));
                        }
                    }
                    push_bytes(&mut segments, &mut length, b"\r\n".to_vec())?;
                }
            }
        }
        push_bytes(
            &mut segments,
            &mut length,
            format!("--{}--\r\n", self.boundary).into_bytes(),
        )?;
        Ok((
            content_type,
            MultipartBody {
                state: Rc::new(RefCell::new(BodyState {
                    segments,
                    failure: None,
                    completed: false,
                })),
                content_length: length,
            },
        ))
    }
}

fn push_bytes(
    segments: &mut VecDeque<Segment>,
    length: &mut Option<usize>,
    bytes: Vec<u8>,
) -> Result<(), MultipartError> {
    if let Some(current) = length {
        *current = current
            .checked_add(bytes.len())
            .ok_or(MultipartError::LengthOverflow)?;
    }
    segments.push_back(Segment::Bytes(bytes));
    Ok(())
}

fn validate_boundary(boundary: &str) -> Result<(), MultipartError> {
    if boundary.is_empty()
        || !boundary
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(MultipartError::InvalidMetadata);
    }
    Ok(())
}

fn validate_quoted(value: &str) -> Result<(), MultipartError> {
    if value.is_empty() || value.contains(['\r', '\n', '"']) {
        return Err(MultipartError::InvalidMetadata);
    }
    Ok(())
}

fn validate_header_value(value: &str) -> Result<(), MultipartError> {
    if value.is_empty() || value.contains(['\r', '\n']) {
        return Err(MultipartError::InvalidMetadata);
    }
    Ok(())
}

#[cfg(test)]
mod tests;

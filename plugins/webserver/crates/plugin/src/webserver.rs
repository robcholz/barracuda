//! One cross-platform picoserve server configured by dependent Plugins.
//!
//! [`WebServer`] owns portable routes only. [`WebServerPlugin`](crate::WebServerPlugin)
//! starts the Embassy task that supplies sockets and a picoserve timer.

use alloc::boxed::Box;
use alloc::rc::{Rc, Weak};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use core::task::Poll;

use async_channel::{Receiver, Sender};
use barracuda_bulk_memory::{BulkBox, BulkText};
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use futures_lite::future;
use picoserve::futures::Either;
use picoserve::io::Write;
use picoserve::io::{Error as _, ErrorKind, Read};
use picoserve::request::{Path, Request};
use picoserve::response::ws::{Message, SocketRx, SocketTx, WebSocketCallback};
use picoserve::response::{Content, IntoResponse, Response, ResponseWriter, StatusCode};
use picoserve::routing::{get, MethodHandler, PathRouterService, Router};
use picoserve::time::Timer;
use picoserve::{Config, DisconnectionInfo, NoGracefulShutdown, Server};

const WEBSOCKET_BUFFER_BYTES: usize = 8 * 1024;
const WEBSOCKET_QUEUE_CAPACITY: usize = 4;

#[repr(align(16))]
struct InlineSpace<const WORDS: usize>([usize; WORDS]);

type ReaderSpace = InlineSpace<16>;
type ReadSpace = InlineSpace<64>;
type InlineFuture<'a, T, S> = smallbox::SmallBox<dyn Future<Output = T> + 'a, S>;

fn inline_future<'a, T, F, S>(value: F) -> InlineFuture<'a, T, S>
where
    F: Future<Output = T> + 'a,
{
    // Reject oversized implementations at compile time, never fall back to the heap.
    const {
        assert!(core::mem::size_of::<F>() <= core::mem::size_of::<S>());
        assert!(core::mem::align_of::<F>() <= core::mem::align_of::<S>());
    }
    smallbox::smallbox!(value)
}

/// Cooperative future returned by a WebSocket endpoint.
pub type WebSocketFuture<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// Cooperative future returned by a portable HTTP endpoint.
pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = HttpResponse> + 'a>>;

/// HTTP method presented to a portable endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum HttpMethod {
    /// GET request.
    Get,
    /// POST request.
    Post,
    /// PUT request.
    Put,
    /// DELETE request.
    Delete,
    /// OPTIONS request.
    Options,
    /// TRACE request.
    Trace,
    /// PATCH request.
    Patch,
    /// A method not represented by this portable contract.
    Other,
}

impl HttpMethod {
    fn from_request(method: &str) -> Self {
        match method {
            "GET" => Self::Get,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "DELETE" => Self::Delete,
            "OPTIONS" => Self::Options,
            "TRACE" => Self::Trace,
            "PATCH" => Self::Patch,
            _ => Self::Other,
        }
    }
}

/// Owned HTTP request passed to a portable endpoint.
pub struct HttpRequest {
    method: HttpMethod,
    path: String,
    body: Vec<u8>,
}

impl HttpRequest {
    /// Creates a request value.
    #[must_use]
    pub const fn new(method: HttpMethod, body: Vec<u8>) -> Self {
        Self {
            method,
            path: String::new(),
            body,
        }
    }

    /// Creates a request with its encoded URL path, excluding the query string.
    #[must_use]
    pub fn with_path(method: HttpMethod, path: String, body: Vec<u8>) -> Self {
        Self { method, path, body }
    }

    /// Returns the encoded path. Synthetic requests created with `new` have an empty path.
    /// Consumers mapping URLs to resources must validate and decode that mapping themselves.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the request method.
    #[must_use]
    pub const fn method(&self) -> HttpMethod {
        self.method
    }

    /// Returns the complete request body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Consumes the request and returns its complete body without copying it.
    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
}

/// Owned HTTP response returned by a portable endpoint.
pub struct HttpResponse {
    status: u16,
    content_type: &'static str,
    body: HttpBody,
}

enum HttpBody {
    Bytes(Vec<u8>),
    Stream {
        length: usize,
        reader: smallbox::SmallBox<dyn BodyReader, ReaderSpace>,
    },
}

trait BodyReader {
    fn read<'a>(
        &'a mut self,
        buffer: &'a mut [u8],
    ) -> InlineFuture<'a, Result<usize, ErrorKind>, ReadSpace>;
}

impl<T: Read> BodyReader for T {
    fn read<'a>(
        &'a mut self,
        buffer: &'a mut [u8],
    ) -> InlineFuture<'a, Result<usize, ErrorKind>, ReadSpace> {
        inline_future(async move { Read::read(self, buffer).await.map_err(|error| error.kind()) })
    }
}

/// Failure producing a streamed HTTP body. The connection is terminated on failure.
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum HttpStreamError {
    /// The producer returned an I/O error.
    #[error("HTTP response source failed: {0:?}")]
    Read(ErrorKind),
    /// The source ended before the declared Content-Length.
    #[error("HTTP response source ended before Content-Length")]
    UnexpectedEof,
    /// The producer violated the Read buffer contract.
    #[error("HTTP response source returned an invalid read length")]
    InvalidRead,
}

impl HttpResponse {
    /// Creates a response from its status, media type, and body.
    #[must_use]
    pub const fn new(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            body: HttpBody::Bytes(body),
        }
    }

    /// Streams exactly `length` bytes from an owned async reader using bounded buffers.
    /// Excess source bytes are not sent. Early EOF or read failure closes the connection.
    /// The reader must fit 16 machine words and its read future 64 machine words,
    /// both with at most machine-word alignment (checked at compile time).
    #[must_use]
    pub fn stream<R: Read + 'static>(
        status: u16,
        content_type: &'static str,
        length: usize,
        reader: R,
    ) -> Self {
        const {
            assert!(core::mem::size_of::<R>() <= core::mem::size_of::<ReaderSpace>());
            assert!(core::mem::align_of::<R>() <= core::mem::align_of::<ReaderSpace>());
        }
        Self {
            status,
            content_type,
            body: HttpBody::Stream {
                length,
                reader: smallbox::smallbox!(reader),
            },
        }
    }

    /// Returns the numeric HTTP status.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Returns the response media type.
    #[must_use]
    pub const fn content_type(&self) -> &'static str {
        self.content_type
    }

    /// Returns buffered response bytes, or `None` for a streaming response.
    #[must_use]
    pub fn body(&self) -> Option<&[u8]> {
        match &self.body {
            HttpBody::Bytes(bytes) => Some(bytes),
            HttpBody::Stream { .. } => None,
        }
    }
}

/// Portable behavior served at one ordinary HTTP route.
pub trait HttpEndpoint: 'static {
    /// Handles one complete request.
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a>;
}

/// A request whose body is read as it arrives, handed to an [`HttpUploadEndpoint`].
///
/// It implements `embedded_io_async::Read` over exactly `content_length` bytes, read
/// from the socket through the connection's own buffers: nothing is collected in memory.
pub struct HttpUpload<'a> {
    method: HttpMethod,
    path: &'a str,
    length: usize,
    body: &'a mut dyn BodyReader,
}

impl HttpUpload<'_> {
    /// Returns the request method.
    #[must_use]
    pub const fn method(&self) -> HttpMethod {
        self.method
    }

    /// Returns the encoded path, without its query.
    /// Consumers mapping URLs to resources must validate and decode that mapping themselves.
    #[must_use]
    pub const fn path(&self) -> &str {
        self.path
    }

    /// Returns the body's declared length in bytes.
    #[must_use]
    pub const fn content_length(&self) -> usize {
        self.length
    }
}

impl picoserve::io::ErrorType for HttpUpload<'_> {
    type Error = ErrorKind;
}

impl Read for HttpUpload<'_> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, ErrorKind> {
        self.body.read(buffer).await
    }
}

/// Portable behavior served at an HTTP subtree whose request bodies are streamed.
///
/// Use it for bodies larger than the connection's request buffer, such as uploads.
/// Bytes of the body the endpoint leaves unread are discarded before the response.
pub trait HttpUploadEndpoint: 'static {
    /// Handles one request, reading its body as it arrives.
    fn handle<'a>(&'a self, upload: HttpUpload<'a>) -> HttpFuture<'a>;
}

/// Resource provider receiving a borrowed encoded URL path (without its query).
/// No request body is collected. Providers choose their own storage and path policy.
/// The returned future must fit 256 machine words with machine-word alignment;
/// incompatible implementations fail to compile rather than allocating.
///
/// ```
/// use barracuda_webserver_plugin::{HttpProvider, HttpResponse, WebServer};
/// struct Assets;
/// impl HttpProvider for Assets {
///     async fn serve(&self, path: &str) -> HttpResponse {
///         match path {
///             "/assets/app.js" => {
///                 let bytes: &'static [u8] = b"console.log('ready');";
///                 HttpResponse::stream(200, "application/javascript", bytes.len(), bytes)
///             }
///             _ => HttpResponse::new(404, "text/plain", Vec::new()),
///         }
///     }
/// }
/// let server = WebServer::new();
/// let registration = server.serve("/assets/*", Assets)?;
/// # Ok::<(), barracuda_webserver_plugin::WebServerError>(())
/// ```
pub trait HttpProvider: 'static {
    /// Opens a resource response. The future is stored inline by the server.
    fn serve(&self, path: &str) -> impl Future<Output = HttpResponse>;
}

trait ErasedProvider<const WORDS: usize> {
    fn serve<'a>(&'a self, path: &'a str) -> InlineFuture<'a, HttpResponse, InlineSpace<WORDS>>;
}

impl<P: HttpProvider, const WORDS: usize> ErasedProvider<WORDS> for P {
    fn serve<'a>(&'a self, path: &'a str) -> InlineFuture<'a, HttpResponse, InlineSpace<WORDS>> {
        inline_future(HttpProvider::serve(self, path))
    }
}

/// Shared leaf provider for aggregators that select a provider before awaiting it.
/// Construction allocates once; cloning and dispatch do not allocate.
/// Leaf futures must fit 128 machine words, leaving room inside the server's
/// 256-word handler storage for the aggregator's own state.
#[derive(Clone)]
pub struct HttpProviderHandle(Rc<dyn ErasedProvider<128>>);

impl HttpProviderHandle {
    /// Captures a concrete leaf provider for shared dispatch.
    pub fn new(provider: impl HttpProvider) -> Self {
        Self(Rc::new(provider))
    }

    /// Runs the selected provider without allocating a future.
    pub async fn serve(&self, path: &str) -> HttpResponse {
        self.0.serve(path).await
    }
}

/// One client message delivered to a registered WebSocket endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebSocketMessage {
    /// UTF-8 text message.
    Text(String),
    /// Opaque binary message.
    Binary(Vec<u8>),
}

/// A WebSocket connection has closed and can no longer transfer messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("WebSocket connection is closed")]
pub struct WebSocketClosed;

/// Message-level connection passed to one portable endpoint.
///
/// picoserve framing and socket errors remain inside [`WebServer`]. Endpoints
/// exchange owned messages so their futures are independent of the Platform's
/// concrete socket types.
pub struct WebSocketConnection {
    incoming: Receiver<WebSocketMessage>,
    outgoing: Sender<BulkText>,
}

impl WebSocketConnection {
    fn new(incoming: Receiver<WebSocketMessage>, outgoing: Sender<BulkText>) -> Self {
        Self { incoming, outgoing }
    }

    /// Waits for the next client message.
    ///
    /// # Errors
    ///
    /// Returns [`WebSocketClosed`] after the underlying socket closes.
    pub async fn receive(&self) -> Result<WebSocketMessage, WebSocketClosed> {
        self.incoming
            .recv()
            .await
            .map_err(|_closed| WebSocketClosed)
    }

    /// Sends one UTF-8 text message to the client.
    ///
    /// Queued messages wait in bulk memory until the socket writes them.
    ///
    /// # Errors
    ///
    /// Returns [`WebSocketClosed`] after the underlying socket closes.
    pub async fn send_text(&self, message: impl Into<BulkText>) -> Result<(), WebSocketClosed> {
        self.outgoing
            .send(message.into())
            .await
            .map_err(|_closed| WebSocketClosed)
    }
}

/// Portable behavior served for each connection to one WebSocket endpoint.
pub trait WebSocketEndpoint: 'static {
    /// Runs one connected client until either side closes.
    fn connected<'a>(&'a self, connection: WebSocketConnection) -> WebSocketFuture<'a>;
}

#[derive(Clone)]
enum Endpoint {
    WebSocket(Rc<dyn WebSocketEndpoint>),
    Http(Rc<dyn HttpEndpoint>),
    Upload(Rc<dyn HttpUploadEndpoint>),
    Provider(Rc<dyn ErasedProvider<256>>),
}

#[derive(Clone)]
struct RegisteredEndpoint {
    path: &'static str,
    prefix: bool,
    endpoint: Endpoint,
}

#[derive(Clone)]
struct EndpointRouter<'a> {
    endpoints: Rc<RefCell<Vec<RegisteredEndpoint>>>,
    stream_error: &'a Cell<Option<HttpStreamError>>,
}

fn resolve(endpoints: &[RegisteredEndpoint], path: &str) -> Option<RegisteredEndpoint> {
    endpoints
        .iter()
        .filter(|entry| {
            if !entry.prefix {
                return entry.path == path;
            }
            path == entry.path
                || path
                    .strip_prefix(entry.path)
                    .is_some_and(|suffix| entry.path.ends_with('/') || suffix.starts_with('/'))
        })
        .max_by_key(|entry| (!entry.prefix, entry.path.len()))
        .cloned()
}

impl PathRouterService for EndpointRouter<'_> {
    async fn call_path_router_service<R, W>(
        &self,
        _state: &(),
        (): (),
        path: Path<'_>,
        mut request: Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error>
    where
        R: picoserve::io::Read,
        W: ResponseWriter<Error = R::Error>,
    {
        let registered = resolve(&self.endpoints.borrow(), path.encoded());
        let Some(registered) = registered else {
            return (StatusCode::NOT_FOUND, "Web endpoint not found")
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        };
        match registered.endpoint.clone() {
            Endpoint::Provider(provider) => {
                let connection = request.body_connection.finalize().await?;
                if request.parts.method() != "GET" {
                    return Response::new(StatusCode::METHOD_NOT_ALLOWED, "Only GET is supported")
                        .with_header("Allow", "GET")
                        .write_to(connection, response_writer)
                        .await;
                }
                let response = provider.serve(path.encoded()).await;
                Response::new(
                    StatusCode::new(response.status),
                    HttpContent {
                        content_type: response.content_type,
                        body: response.body,
                        stream_error: self.stream_error,
                    },
                )
                .write_to(connection, response_writer)
                .await
            }
            Endpoint::WebSocket(endpoint) => {
                let callback = EndpointCallback { endpoint };
                get(move |upgrade: picoserve::response::ws::WebSocketUpgrade| {
                    let callback = callback.clone();
                    async move { upgrade.on_upgrade(callback) }
                })
                .call_method_handler(&(), (), request, response_writer)
                .await
            }
            Endpoint::Upload(endpoint) => {
                let method = HttpMethod::from_request(request.parts.method());
                let response = {
                    let mut reader = request.body_connection.body().reader();
                    let length = reader.content_length();
                    endpoint
                        .handle(HttpUpload {
                            method,
                            path: path.encoded(),
                            length,
                            body: &mut reader,
                        })
                        .await
                };
                Response::new(
                    StatusCode::new(response.status),
                    HttpContent {
                        content_type: response.content_type,
                        body: response.body,
                        stream_error: self.stream_error,
                    },
                )
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await
            }
            Endpoint::Http(endpoint) => {
                let method = HttpMethod::from_request(request.parts.method());
                let body = match request.body_connection.body().read_all().await {
                    Ok(body) => body.to_vec(),
                    Err(error) => {
                        return error
                            .write_to(request.body_connection.finalize().await?, response_writer)
                            .await;
                    }
                };
                let response = endpoint
                    .handle(HttpRequest::with_path(
                        method,
                        path.encoded().to_string(),
                        body,
                    ))
                    .await;
                let response = Response::new(
                    StatusCode::new(response.status),
                    HttpContent {
                        content_type: response.content_type,
                        body: response.body,
                        stream_error: self.stream_error,
                    },
                );
                response
                    .write_to(request.body_connection.finalize().await?, response_writer)
                    .await
            }
        }
    }
}

struct HttpContent<'a> {
    content_type: &'static str,
    body: HttpBody,
    stream_error: &'a Cell<Option<HttpStreamError>>,
}

impl Content for HttpContent<'_> {
    fn content_type(&self) -> &'static str {
        self.content_type
    }

    fn content_length(&self) -> usize {
        match &self.body {
            HttpBody::Bytes(bytes) => bytes.len(),
            HttpBody::Stream { length, .. } => *length,
        }
    }

    async fn write_content<W: Write>(self, mut writer: W) -> Result<(), W::Error> {
        match self.body {
            HttpBody::Bytes(bytes) => bytes.write_content(writer).await,
            HttpBody::Stream { length, mut reader } => {
                let mut remaining = length;
                let mut buffer = [0_u8; 1024];
                while remaining > 0 {
                    let capacity = remaining.min(buffer.len());
                    let result = match reader.read(&mut buffer[..capacity]).await {
                        Ok(0) => Err(HttpStreamError::UnexpectedEof),
                        Ok(count) if count <= capacity => Ok(count),
                        Ok(_) => Err(HttpStreamError::InvalidRead),
                        Err(error) => Err(HttpStreamError::Read(error)),
                    };
                    let count = match result {
                        Ok(count) => count,
                        Err(error) => {
                            // Wake serve_connection, which drops this response and its socket.
                            self.stream_error.set(Some(error));
                            return core::future::pending().await;
                        }
                    };
                    writer.write_all(&buffer[..count]).await?;
                    remaining -= count;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone)]
struct EndpointCallback {
    endpoint: Rc<dyn WebSocketEndpoint>,
}

impl WebSocketCallback for EndpointCallback {
    async fn run<R: picoserve::io::Read, W: picoserve::io::Write<Error = R::Error>>(
        self,
        rx: SocketRx<R>,
        tx: SocketTx<W>,
    ) -> Result<(), W::Error> {
        let (incoming_tx, incoming_rx) = async_channel::bounded(WEBSOCKET_QUEUE_CAPACITY);
        let (outgoing_tx, outgoing_rx) = async_channel::bounded(WEBSOCKET_QUEUE_CAPACITY);
        let connection = WebSocketConnection::new(incoming_rx, outgoing_tx);
        let endpoint = self.endpoint.connected(connection);
        let socket = drive_socket(rx, tx, incoming_tx, outgoing_rx);
        let (_, result) = future::zip(endpoint, socket).await;
        result
    }
}

async fn drive_socket<R, W>(
    mut rx: SocketRx<R>,
    mut tx: SocketTx<W>,
    incoming: Sender<WebSocketMessage>,
    outgoing: Receiver<BulkText>,
) -> Result<(), W::Error>
where
    R: picoserve::io::Read,
    W: picoserve::io::Write<Error = R::Error>,
{
    let mut buffer = BulkBox::<[u8]>::new_zeroed_slice(WEBSOCKET_BUFFER_BYTES);
    loop {
        match rx.next_message(&mut buffer, outgoing.recv()).await? {
            Either::First(Ok(Message::Text(text))) => {
                if incoming
                    .send(WebSocketMessage::Text(text.to_string()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Either::First(Ok(Message::Binary(bytes))) => {
                if incoming
                    .send(WebSocketMessage::Binary(bytes.to_vec()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Either::First(Ok(Message::Ping(data))) => tx.send_pong(data).await?,
            Either::First(Ok(Message::Close(_))) => break,
            Either::First(Ok(Message::Pong(_))) => {}
            Either::First(Err(_frame)) => break,
            Either::Second(Err(_closed)) => break,
            Either::Second(Ok(message)) => tx.send_text(message.as_str()).await?,
        }
    }
    tx.close(None).await
}

/// Failure while registering an endpoint with the global Web server.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum WebServerError {
    /// An endpoint path must be absolute.
    #[error("Web endpoint path must start with '/'")]
    InvalidPath,
    /// An endpoint is already registered at this path.
    #[error("WebServer endpoint path is already registered")]
    DuplicatePath,
    /// The single additional listener slot is already registered.
    #[error("WebServer additional listener capacity is exhausted")]
    AdditionalListenerCapacity,
}

/// Scoped registration of one WebSocket or ordinary HTTP endpoint.
///
/// Dropping this value removes the endpoint, so Plugins should retain it for
/// their own lifecycle with `PluginRegisterContext::retain`.
#[must_use = "dropping the registration immediately removes the endpoint"]
pub struct WebRouteRegistration {
    endpoints: Weak<RefCell<Vec<RegisteredEndpoint>>>,
    path: &'static str,
    prefix: bool,
}

impl Drop for WebRouteRegistration {
    fn drop(&mut self) {
        if let Some(endpoints) = self.endpoints.upgrade() {
            endpoints
                .borrow_mut()
                .retain(|endpoint| endpoint.path != self.path || endpoint.prefix != self.prefix);
        }
    }
}

pub(crate) struct WebListener {
    pub(crate) stack: Stack<'static>,
    pub(crate) port: u16,
    pub(crate) stopped: Rc<Signal<NoopRawMutex, ()>>,
}

impl Clone for WebListener {
    fn clone(&self) -> Self {
        Self {
            stack: self.stack,
            port: self.port,
            stopped: Rc::clone(&self.stopped),
        }
    }
}

/// Scoped registration of one additional network interface listener.
///
/// Dropping this value stops the listener. The WebServer Plugin owns the
/// listener task; the dependent Plugin owns only the registration lifetime.
#[must_use = "dropping the registration immediately stops the listener"]
pub struct WebListenerRegistration {
    listeners: Weak<RefCell<Vec<WebListener>>>,
    stopped: Rc<Signal<NoopRawMutex, ()>>,
}

impl Drop for WebListenerRegistration {
    fn drop(&mut self) {
        self.stopped.signal(());
        if let Some(listeners) = self.listeners.upgrade() {
            listeners
                .borrow_mut()
                .retain(|listener| !Rc::ptr_eq(&listener.stopped, &self.stopped));
        }
    }
}

/// Failure while serving one Platform-provided connection.
#[derive(Debug, thiserror::Error)]
pub enum ServeConnectionError<E: picoserve::io::Error + 'static> {
    /// A response source failed; the connection has been dropped.
    #[error(transparent)]
    ResponseSource(#[from] HttpStreamError),
    /// No Plugin registered an endpoint before the connection was served.
    #[error("WebServer has no registered endpoint")]
    EndpointMissing,
    /// picoserve failed while handling the connection.
    #[error(transparent)]
    Connection(#[from] picoserve::Error<E>),
}

/// The single cross-platform Web server shared by registered Plugins.
pub struct WebServer {
    endpoints: Rc<RefCell<Vec<RegisteredEndpoint>>>,
    listeners: Rc<RefCell<Vec<WebListener>>>,
    config: Config,
}

impl WebServer {
    /// Creates an unconfigured server with picoserve's default connection policy.
    #[must_use]
    pub fn new() -> Self {
        Config::const_default().into()
    }

    /// Registers the same route table on an additional Platform network stack.
    ///
    /// The WebServer Plugin starts and owns one single-connection listener for
    /// this interface after every Plugin has registered. Retain the returned
    /// guard for as long as the interface should accept HTTP connections.
    ///
    /// # Errors
    ///
    /// Returns [`WebServerError::AdditionalListenerCapacity`] when another
    /// Plugin already owns the single additional listener slot.
    pub fn listen_on_stack(
        &self,
        stack: Stack<'static>,
        port: u16,
    ) -> Result<WebListenerRegistration, WebServerError> {
        if !self.listeners.borrow().is_empty() {
            return Err(WebServerError::AdditionalListenerCapacity);
        }
        let stopped = Rc::new(Signal::new());
        self.listeners.borrow_mut().push(WebListener {
            stack,
            port,
            stopped: Rc::clone(&stopped),
        });
        Ok(WebListenerRegistration {
            listeners: Rc::downgrade(&self.listeners),
            stopped,
        })
    }

    pub(crate) fn listeners(&self) -> Vec<WebListener> {
        self.listeners.borrow().clone()
    }

    /// Registers one portable WebSocket endpoint during Plugin registration.
    ///
    /// # Errors
    ///
    /// Returns a scoped registration that removes the endpoint when dropped.
    /// Returns an error for a relative path or a duplicate path.
    pub fn serve_websocket<E>(
        &self,
        path: &'static str,
        endpoint: E,
    ) -> Result<WebRouteRegistration, WebServerError>
    where
        E: WebSocketEndpoint,
    {
        if !path.starts_with('/') {
            return Err(WebServerError::InvalidPath);
        }
        let mut endpoints = self.endpoints.borrow_mut();
        if endpoints
            .iter()
            .any(|endpoint| endpoint.path == path && !endpoint.prefix)
        {
            return Err(WebServerError::DuplicatePath);
        }
        endpoints.push(RegisteredEndpoint {
            path,
            prefix: false,
            endpoint: Endpoint::WebSocket(Rc::new(endpoint)),
        });
        Ok(WebRouteRegistration {
            endpoints: Rc::downgrade(&self.endpoints),
            path,
            prefix: false,
        })
    }

    /// Registers a resource provider at an exact path or a trailing `/*` subtree.
    /// Exact routes take priority. Keep the returned guard for the Plugin lifetime.
    ///
    /// # Errors
    /// Rejects relative paths, misplaced wildcards, and duplicate registrations.
    pub fn serve<P: HttpProvider>(
        &self,
        pattern: &'static str,
        provider: P,
    ) -> Result<WebRouteRegistration, WebServerError> {
        let prefix = pattern.ends_with("/*");
        let path = if prefix {
            &pattern[..pattern.len() - 1]
        } else {
            pattern
        };
        if !path.starts_with('/') || path.contains('*') {
            return Err(WebServerError::InvalidPath);
        }
        let mut endpoints = self.endpoints.borrow_mut();
        if endpoints
            .iter()
            .any(|entry| entry.path == path && entry.prefix == prefix)
        {
            return Err(WebServerError::DuplicatePath);
        }
        endpoints.push(RegisteredEndpoint {
            path,
            prefix,
            endpoint: Endpoint::Provider(Rc::new(provider)),
        });
        Ok(WebRouteRegistration {
            endpoints: Rc::downgrade(&self.endpoints),
            path,
            prefix,
        })
    }

    /// Registers one portable ordinary HTTP endpoint during Plugin registration.
    ///
    /// # Errors
    ///
    /// Returns a scoped registration that removes the endpoint when dropped.
    /// Returns an error for a relative path or a duplicate path.
    pub fn serve_http<E>(
        &self,
        path: &'static str,
        endpoint: E,
    ) -> Result<WebRouteRegistration, WebServerError>
    where
        E: HttpEndpoint,
    {
        self.register_http(path, false, endpoint)
    }

    /// Registers an HTTP subtree. Exact routes win, followed by the longest matching prefix.
    /// `/assets` matches itself and `/assets/...`, but not `/assets-other`.
    /// `/` is a fallback for every path without an exact or more specific prefix route.
    ///
    /// # Errors
    /// Rejects relative paths and duplicate prefixes. Exact and prefix routes may coexist.
    pub fn serve_http_prefix<E: HttpEndpoint>(
        &self,
        path: &'static str,
        endpoint: E,
    ) -> Result<WebRouteRegistration, WebServerError> {
        self.register_http(path, true, endpoint)
    }

    /// Registers an HTTP subtree whose request bodies are streamed to the endpoint
    /// instead of collected. It routes like [`Self::serve_http_prefix`].
    ///
    /// # Errors
    /// Rejects relative paths and duplicate prefixes.
    pub fn serve_upload<E: HttpUploadEndpoint>(
        &self,
        path: &'static str,
        endpoint: E,
    ) -> Result<WebRouteRegistration, WebServerError> {
        self.register_route(path, true, Endpoint::Upload(Rc::new(endpoint)))
    }

    fn register_http<E: HttpEndpoint>(
        &self,
        path: &'static str,
        prefix: bool,
        endpoint: E,
    ) -> Result<WebRouteRegistration, WebServerError> {
        self.register_route(path, prefix, Endpoint::Http(Rc::new(endpoint)))
    }

    fn register_route(
        &self,
        path: &'static str,
        prefix: bool,
        endpoint: Endpoint,
    ) -> Result<WebRouteRegistration, WebServerError> {
        if !path.starts_with('/') {
            return Err(WebServerError::InvalidPath);
        }
        let mut endpoints = self.endpoints.borrow_mut();
        if endpoints
            .iter()
            .any(|endpoint| endpoint.path == path && endpoint.prefix == prefix)
        {
            return Err(WebServerError::DuplicatePath);
        }
        endpoints.push(RegisteredEndpoint {
            path,
            prefix,
            endpoint,
        });
        Ok(WebRouteRegistration {
            endpoints: Rc::downgrade(&self.endpoints),
            path,
            prefix,
        })
    }

    /// Serves one connected Platform socket using a Platform timer.
    ///
    /// The caller owns listener setup, connection acceptance, executor policy,
    /// and the request buffer. Call this once for every accepted connection.
    ///
    /// # Errors
    ///
    /// Returns an error when no Plugin registered an endpoint or picoserve
    /// cannot serve the connection.
    pub async fn serve_connection<Runtime, T, S>(
        &self,
        timer: T,
        http_buffer: &mut [u8],
        socket: S,
    ) -> Result<DisconnectionInfo<NoGracefulShutdown>, ServeConnectionError<S::Error>>
    where
        T: Timer<Runtime>,
        S: picoserve::io::Socket<Runtime>,
    {
        if self.endpoints.borrow().is_empty() {
            return Err(ServeConnectionError::EndpointMissing);
        }
        let stream_error = Cell::new(None);
        let app = Router::from_service(EndpointRouter {
            endpoints: Rc::clone(&self.endpoints),
            stream_error: &stream_error,
        });
        let server = Server::custom(&app, timer, &self.config, http_buffer);
        let error =
            core::future::poll_fn(|_| stream_error.take().map_or(Poll::Pending, Poll::Ready));
        // select polls the server first, then observes its stack-local error slot.
        match embassy_futures::select::select(server.serve(socket), error).await {
            embassy_futures::select::Either::First(result) => {
                result.map_err(ServeConnectionError::Connection)
            }
            embassy_futures::select::Either::Second(error) => Err(error.into()),
        }
    }
}

impl Default for WebServer {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Config> for WebServer {
    fn from(config: Config) -> Self {
        Self {
            endpoints: Rc::new(RefCell::new(Vec::new())),
            listeners: Rc::new(RefCell::new(Vec::new())),
            config,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use super::{
        HttpEndpoint, HttpFuture, HttpRequest, HttpResponse, WebServer, WebServerError,
        WebSocketConnection, WebSocketEndpoint, WebSocketFuture,
    };

    struct EmptyEndpoint;

    #[test]
    fn prefix_routes_use_segments_and_exact_routes_win() {
        let server = WebServer::new();
        let _root = server
            .serve_http_prefix("/", EmptyHttpEndpoint)
            .expect("root");
        let assets = server
            .serve_http_prefix("/assets", EmptyHttpEndpoint)
            .expect("assets");
        let exact = server
            .serve_http("/assets", EmptyHttpEndpoint)
            .expect("exact");
        assert!(
            !super::resolve(&server.endpoints.borrow(), "/assets")
                .expect("match")
                .prefix
        );
        assert_eq!(
            super::resolve(&server.endpoints.borrow(), "/assets/file.js")
                .expect("match")
                .path,
            "/assets"
        );
        assert_eq!(
            super::resolve(&server.endpoints.borrow(), "/assets-other")
                .expect("match")
                .path,
            "/"
        );
        drop(exact);
        assert!(
            super::resolve(&server.endpoints.borrow(), "/assets")
                .expect("match")
                .prefix
        );
        drop(assets);
        assert_eq!(
            super::resolve(&server.endpoints.borrow(), "/assets/file.js")
                .expect("match")
                .path,
            "/"
        );
        assert!(server
            .serve_http_prefix("relative", EmptyHttpEndpoint)
            .is_err());
        assert!(server.serve_http_prefix("/", EmptyHttpEndpoint).is_err());
    }

    impl WebSocketEndpoint for EmptyEndpoint {
        fn connected<'a>(&'a self, _connection: WebSocketConnection) -> WebSocketFuture<'a> {
            Box::pin(async {})
        }
    }

    struct EmptyHttpEndpoint;

    impl HttpEndpoint for EmptyHttpEndpoint {
        fn handle<'a>(&'a self, _request: HttpRequest) -> HttpFuture<'a> {
            Box::pin(async { HttpResponse::new(204, "application/json", Vec::new()) })
        }
    }

    #[test]
    fn registers_unique_absolute_endpoints() {
        let server = WebServer::new();

        assert!(matches!(
            server.serve_websocket("relative", EmptyEndpoint),
            Err(WebServerError::InvalidPath)
        ));
        let _root = server
            .serve_websocket("/", EmptyEndpoint)
            .expect("register root");
        let second = server
            .serve_websocket("/second", EmptyEndpoint)
            .expect("register second endpoint");
        assert!(matches!(
            server.serve_websocket("/second", EmptyEndpoint),
            Err(WebServerError::DuplicatePath)
        ));
        assert!(matches!(
            server.serve_http("relative-http", EmptyHttpEndpoint),
            Err(WebServerError::InvalidPath)
        ));
        assert!(matches!(
            server.serve_http("/second", EmptyHttpEndpoint),
            Err(WebServerError::DuplicatePath)
        ));

        drop(second);
        let _replacement = server
            .serve_websocket("/second", EmptyEndpoint)
            .expect("register released endpoint");
    }
}

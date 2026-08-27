//! One cross-platform picoserve server configured by dependent Plugins.
//!
//! [`WebServer`] owns portable routes only. [`WebServerPlugin`](crate::WebServerPlugin)
//! starts the Embassy task that supplies sockets and a picoserve timer.

use alloc::boxed::Box;
use alloc::rc::{Rc, Weak};
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;

use async_channel::{Receiver, Sender};
use futures_lite::future;
use picoserve::futures::Either;
use picoserve::io::Write;
use picoserve::request::{Path, Request};
use picoserve::response::ws::{Message, SocketRx, SocketTx, WebSocketCallback};
use picoserve::response::{Content, IntoResponse, Response, ResponseWriter, StatusCode};
use picoserve::routing::{get, MethodHandler, PathRouterService, Router};
use picoserve::time::Timer;
use picoserve::{Config, DisconnectionInfo, NoGracefulShutdown, Server};

const WEBSOCKET_BUFFER_BYTES: usize = 8 * 1024;
const WEBSOCKET_QUEUE_CAPACITY: usize = 4;

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
    body: Vec<u8>,
}

impl HttpRequest {
    /// Creates a request value.
    #[must_use]
    pub const fn new(method: HttpMethod, body: Vec<u8>) -> Self {
        Self { method, body }
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
}

/// Owned HTTP response returned by a portable endpoint.
pub struct HttpResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl HttpResponse {
    /// Creates a response from its status, media type, and body.
    #[must_use]
    pub const fn new(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            body,
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

    /// Returns the response body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Portable behavior served at one ordinary HTTP route.
pub trait HttpEndpoint: 'static {
    /// Handles one complete request.
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a>;
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
    outgoing: Sender<String>,
}

impl WebSocketConnection {
    fn new(incoming: Receiver<WebSocketMessage>, outgoing: Sender<String>) -> Self {
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
    /// # Errors
    ///
    /// Returns [`WebSocketClosed`] after the underlying socket closes.
    pub async fn send_text(&self, message: impl Into<String>) -> Result<(), WebSocketClosed> {
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
}

#[derive(Clone)]
struct RegisteredEndpoint {
    path: &'static str,
    endpoint: Endpoint,
}

#[derive(Clone)]
struct EndpointRouter {
    endpoints: Vec<RegisteredEndpoint>,
}

impl PathRouterService for EndpointRouter {
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
        let Some(registered) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.path == path.encoded())
        else {
            return (StatusCode::NOT_FOUND, "Web endpoint not found")
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        };
        match registered.endpoint.clone() {
            Endpoint::WebSocket(endpoint) => {
                let callback = EndpointCallback { endpoint };
                get(move |upgrade: picoserve::response::ws::WebSocketUpgrade| {
                    let callback = callback.clone();
                    async move { upgrade.on_upgrade(callback) }
                })
                .call_method_handler(&(), (), request, response_writer)
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
                let response = endpoint.handle(HttpRequest::new(method, body)).await;
                let response = Response::new(
                    StatusCode::new(response.status),
                    HttpContent {
                        content_type: response.content_type,
                        body: response.body,
                    },
                );
                response
                    .write_to(request.body_connection.finalize().await?, response_writer)
                    .await
            }
        }
    }
}

struct HttpContent {
    content_type: &'static str,
    body: Vec<u8>,
}

impl Content for HttpContent {
    fn content_type(&self) -> &'static str {
        self.content_type
    }

    fn content_length(&self) -> usize {
        self.body.len()
    }

    async fn write_content<W: Write>(self, writer: W) -> Result<(), W::Error> {
        self.body.write_content(writer).await
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
    outgoing: Receiver<String>,
) -> Result<(), W::Error>
where
    R: picoserve::io::Read,
    W: picoserve::io::Write<Error = R::Error>,
{
    let mut buffer = vec![0_u8; WEBSOCKET_BUFFER_BYTES];
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
            Either::Second(Ok(message)) => tx.send_text(&message).await?,
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
}

/// Scoped registration of one WebSocket or ordinary HTTP endpoint.
///
/// Dropping this value removes the endpoint, so Plugins should retain it for
/// their own lifecycle with `PluginRegisterContext::retain`.
#[must_use = "dropping the registration immediately removes the endpoint"]
pub struct WebRouteRegistration {
    endpoints: Weak<RefCell<Vec<RegisteredEndpoint>>>,
    path: &'static str,
}

impl Drop for WebRouteRegistration {
    fn drop(&mut self) {
        if let Some(endpoints) = self.endpoints.upgrade() {
            endpoints
                .borrow_mut()
                .retain(|endpoint| endpoint.path != self.path);
        }
    }
}

/// Failure while serving one Platform-provided connection.
#[derive(Debug, thiserror::Error)]
pub enum ServeConnectionError<E: picoserve::io::Error + 'static> {
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
    config: Config,
}

impl WebServer {
    /// Creates an unconfigured server with picoserve's default connection policy.
    #[must_use]
    pub fn new() -> Self {
        Self {
            endpoints: Rc::new(RefCell::new(Vec::new())),
            config: Config::const_default(),
        }
    }

    /// Registers one portable WebSocket endpoint during Plugin registration.
    ///
    /// # Errors
    ///
    /// Returns a scoped registration that removes the endpoint when dropped.
    /// Returns an error for a relative path or a duplicate path.
    pub fn serve<E>(
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
        if endpoints.iter().any(|endpoint| endpoint.path == path) {
            return Err(WebServerError::DuplicatePath);
        }
        endpoints.push(RegisteredEndpoint {
            path,
            endpoint: Endpoint::WebSocket(Rc::new(endpoint)),
        });
        Ok(WebRouteRegistration {
            endpoints: Rc::downgrade(&self.endpoints),
            path,
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
        if !path.starts_with('/') {
            return Err(WebServerError::InvalidPath);
        }
        let mut endpoints = self.endpoints.borrow_mut();
        if endpoints.iter().any(|endpoint| endpoint.path == path) {
            return Err(WebServerError::DuplicatePath);
        }
        endpoints.push(RegisteredEndpoint {
            path,
            endpoint: Endpoint::Http(Rc::new(endpoint)),
        });
        Ok(WebRouteRegistration {
            endpoints: Rc::downgrade(&self.endpoints),
            path,
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
        let endpoints = self.endpoints.borrow().clone();
        if endpoints.is_empty() {
            return Err(ServeConnectionError::EndpointMissing);
        }
        let app = Router::from_service(EndpointRouter { endpoints });
        Server::custom(&app, timer, &self.config, http_buffer)
            .serve(socket)
            .await
            .map_err(ServeConnectionError::Connection)
    }
}

impl Default for WebServer {
    fn default() -> Self {
        Self::new()
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
            server.serve("relative", EmptyEndpoint),
            Err(WebServerError::InvalidPath)
        ));
        let _root = server.serve("/", EmptyEndpoint).expect("register root");
        let second = server
            .serve("/second", EmptyEndpoint)
            .expect("register second endpoint");
        assert!(matches!(
            server.serve("/second", EmptyEndpoint),
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
            .serve("/second", EmptyEndpoint)
            .expect("register released endpoint");
    }
}

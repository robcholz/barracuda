use alloc::{boxed::Box, rc::Rc, string::String};

use embassy_net::{
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
    Stack,
};
use embedded_nal_async::{Dns, TcpConnect};

use crate::{
    reqwless_client::ReqwlessClient, Body, HttpClient, HttpFuture, Method, Request, ResponseStream,
};

const TCP_CONNECTIONS: usize = 4;
const TCP_TX_BYTES: usize = 4 * 1024;
const TCP_RX_BYTES: usize = 4 * 1024;

type PlatformTcpClient = TcpClient<'static, TCP_CONNECTIONS, TCP_TX_BYTES, TCP_RX_BYTES>;
type PlatformDnsResolver = DnsSocket<'static>;
type PlatformBackend = ReqwlessClient<'static, PlatformTcpClient, PlatformDnsResolver>;

/// Cloneable, implementation-independent HTTP client used by components.
///
/// Components use the fluent request methods on this type and never need to
/// know which HTTP engine, TCP implementation, DNS resolver, or buffers back it.
#[derive(Clone)]
pub struct Client<'a> {
    backend: Rc<dyn HttpClient + 'a>,
}

impl<'a> Client<'a> {
    /// Wraps one HTTP backend behind the workspace-wide client facade.
    #[must_use]
    pub fn from_backend(backend: impl HttpClient + 'a) -> Self {
        Self {
            backend: Rc::new(backend),
        }
    }

    /// Creates a client directly over transport contracts.
    ///
    /// Platform composition should normally use [`ClientFactory`]. This
    /// constructor exists for small integrations and scripted network tests;
    /// the concrete HTTP engine remains private to this crate.
    #[must_use]
    pub fn from_network<Tcp, Resolver>(tcp: &'a Tcp, resolver: &'a Resolver) -> Self
    where
        Tcp: TcpConnect + 'a,
        Resolver: Dns + 'a,
    {
        Self::from_backend(ReqwlessClient::new(tcp, resolver))
    }

    /// Creates a transport-backed client with caller-selected buffer sizes.
    ///
    /// This is primarily useful for deterministic tests that exercise response
    /// chunk boundaries. Production composition should use [`ClientFactory`].
    #[must_use]
    pub fn from_network_with_buffer_sizes<Tcp, Resolver>(
        tcp: &'a Tcp,
        resolver: &'a Resolver,
        header_buffer_size: usize,
        read_buffer_size: usize,
    ) -> Self
    where
        Tcp: TcpConnect + 'a,
        Resolver: Dns + 'a,
    {
        Self::from_backend(ReqwlessClient::with_buffer_sizes(
            tcp,
            resolver,
            header_buffer_size,
            read_buffer_size,
        ))
    }

    /// Starts an HTTP request.
    #[must_use]
    pub fn request(&self, method: Method, url: impl Into<String>) -> RequestBuilder<'_, 'a> {
        RequestBuilder {
            client: self,
            request: Request::new(method, url),
        }
    }

    /// Starts a GET request.
    #[must_use]
    pub fn get(&self, url: impl Into<String>) -> RequestBuilder<'_, 'a> {
        self.request(Method::Get, url)
    }

    /// Starts a POST request.
    #[must_use]
    pub fn post(&self, url: impl Into<String>) -> RequestBuilder<'_, 'a> {
        self.request(Method::Post, url)
    }

    /// Starts a PUT request.
    #[must_use]
    pub fn put(&self, url: impl Into<String>) -> RequestBuilder<'_, 'a> {
        self.request(Method::Put, url)
    }

    /// Starts a DELETE request.
    #[must_use]
    pub fn delete(&self, url: impl Into<String>) -> RequestBuilder<'_, 'a> {
        self.request(Method::Delete, url)
    }

    /// Starts a PATCH request.
    #[must_use]
    pub fn patch(&self, url: impl Into<String>) -> RequestBuilder<'_, 'a> {
        self.request(Method::Patch, url)
    }
}

impl HttpClient for Client<'_> {
    fn execute(&self, request: Request) -> HttpFuture<'_> {
        self.backend.execute(request)
    }

    fn execute_stream(&self, request: Request) -> ResponseStream<'_> {
        self.backend.execute_stream(request)
    }
}

/// Fluent request owned by a [`Client`].
pub struct RequestBuilder<'client, 'backend> {
    client: &'client Client<'backend>,
    request: Request,
}

impl<'client> RequestBuilder<'client, '_> {
    /// Adds one request header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.request = self.request.header(name, value);
        self
    }

    /// Sets the request body.
    #[must_use]
    pub fn body(mut self, body: Body) -> Self {
        self.request = self.request.body(body);
        self
    }

    /// Sets an in-memory request body.
    #[must_use]
    pub fn bytes(mut self, body: impl Into<alloc::vec::Vec<u8>>) -> Self {
        self.request = self.request.bytes(body);
        self
    }

    /// Sets a JSON request body and content type.
    #[must_use]
    pub fn json(mut self, body: impl Into<String>) -> Self {
        self.request = self.request.json(body);
        self
    }

    /// Sends the request and buffers the complete response body.
    pub fn send(self) -> HttpFuture<'client> {
        self.client.execute(self.request)
    }

    /// Sends the request and yields the response head and body chunks.
    pub fn send_stream(self) -> ResponseStream<'client> {
        self.client.execute_stream(self.request)
    }
}

enum TlsMode {
    Plaintext,
    #[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
    Config(Rc<dyn Fn() -> Option<crate::TlsConfig<'static>>>),
}

/// Process-lifetime factory for HTTP clients over one Embassy IP stack.
///
/// It owns TCP/DNS setup once. Each call to [`Self::create`] returns an
/// independent HTTP connection owner while sharing the Platform network pool.
#[derive(Clone)]
pub struct ClientFactory {
    tcp: &'static PlatformTcpClient,
    resolver: &'static PlatformDnsResolver,
    tls: Rc<TlsMode>,
    header_buffer_size: usize,
    read_buffer_size: usize,
}

impl ClientFactory {
    /// Creates an explicitly plaintext factory, primarily for tests and
    /// deliberately HTTP-only deployments.
    #[must_use]
    pub fn plaintext(stack: Stack<'static>) -> Self {
        Self::from_parts(stack, TlsMode::Plaintext)
    }

    /// Creates an HTTPS factory from a Platform-owned TLS configuration source.
    #[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
    #[must_use]
    pub fn new(
        stack: Stack<'static>,
        tls: impl Fn() -> Option<crate::TlsConfig<'static>> + 'static,
    ) -> Self {
        Self::from_parts(stack, TlsMode::Config(Rc::new(tls)))
    }

    fn from_parts(stack: Stack<'static>, tls: TlsMode) -> Self {
        let state = Box::leak(Box::new(TcpClientState::new()));
        let tcp = Box::leak(Box::new(TcpClient::new(stack, state)));
        let resolver = Box::leak(Box::new(DnsSocket::new(stack)));
        Self {
            tcp,
            resolver,
            tls: Rc::new(tls),
            header_buffer_size: crate::reqwless_client::DEFAULT_HEADER_BUFFER_SIZE,
            read_buffer_size: crate::reqwless_client::DEFAULT_READ_BUFFER_SIZE,
        }
    }

    /// Overrides per-client HTTP buffer sizes.
    #[must_use]
    pub fn with_buffer_sizes(mut self, header_buffer_size: usize, read_buffer_size: usize) -> Self {
        self.header_buffer_size = header_buffer_size;
        self.read_buffer_size = read_buffer_size;
        self
    }

    /// Creates one independently reusable HTTP client.
    #[must_use]
    pub fn create(&self) -> Client<'static> {
        let backend: PlatformBackend = match self.tls.as_ref() {
            TlsMode::Plaintext => ReqwlessClient::with_buffer_sizes(
                self.tcp,
                self.resolver,
                self.header_buffer_size,
                self.read_buffer_size,
            ),
            #[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
            TlsMode::Config(tls) => match tls() {
                Some(config) => ReqwlessClient::with_tls_and_buffer_sizes(
                    self.tcp,
                    self.resolver,
                    config,
                    self.header_buffer_size,
                    self.read_buffer_size,
                ),
                None => ReqwlessClient::with_buffer_sizes(
                    self.tcp,
                    self.resolver,
                    self.header_buffer_size,
                    self.read_buffer_size,
                ),
            },
        };
        Client::from_backend(backend)
    }
}

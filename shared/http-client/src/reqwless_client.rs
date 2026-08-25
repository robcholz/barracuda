use alloc::{
    boxed::Box,
    rc::Rc,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{cell::RefCell, future::poll_fn, future::Future, pin::Pin};

use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embedded_io::Error as _;
use embedded_io_async::Read as _;
use embedded_nal_async::{Dns, TcpConnect};
use ouroboros::self_referencing;
use reqwless::{
    client::{HttpClient as ReqwlessHttpClient, HttpResource},
    request::{Method as ReqwlessMethod, RequestBody, RequestBuilder as _},
};

use barracuda_runtime_utils::yield_stream::try_yield_stream;

use crate::{
    Body, BodyError, BodyStream, HttpClient, HttpFuture, Method, Request, Response, ResponsePart,
    ResponseStream,
};

#[cfg(all(feature = "embedded-tls", feature = "mbedtls"))]
compile_error!("select exactly one http-client TLS backend");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTPS requested without a TLS configuration")]
    TlsNotConfigured,
    #[error("invalid URL")]
    InvalidUrl,
    #[error("invalid HTTP header")]
    InvalidHeader,
    #[error("connection was aborted")]
    ConnectionAborted,
    #[error(transparent)]
    Body(#[from] BodyError),
    #[error(transparent)]
    Reqwless(#[from] reqwless::Error),
}

impl Error {
    /// Whether retrying the request may recover from this transport failure.
    #[must_use]
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::ConnectionAborted
                | Self::Reqwless(
                    reqwless::Error::Dns
                        | reqwless::Error::Network(_)
                        | reqwless::Error::ConnectionAborted
                )
        )
    }
}

struct StreamBodyState {
    stream: BodyStream,
    failure: Option<BodyError>,
    completed: bool,
}

#[derive(Clone)]
struct StreamBody {
    state: Rc<RefCell<StreamBodyState>>,
    content_length: Option<usize>,
}

impl StreamBody {
    fn new(stream: BodyStream, content_length: Option<usize>) -> Self {
        Self {
            state: Rc::new(RefCell::new(StreamBodyState {
                stream,
                failure: None,
                completed: false,
            })),
            content_length,
        }
    }

    fn failure(&self) -> Option<BodyError> {
        self.state.borrow().failure.clone()
    }
}

impl RequestBody for StreamBody {
    fn len(&self) -> Option<usize> {
        self.content_length
    }

    async fn write<W: embedded_io_async::Write>(&self, writer: &mut W) -> Result<(), W::Error> {
        if self.state.borrow().completed {
            return Ok(());
        }
        loop {
            let next =
                poll_fn(|context| self.state.borrow_mut().stream.as_mut().poll_next(context)).await;
            match next {
                Some(Ok(chunk)) => writer.write_all(&chunk).await?,
                Some(Err(error)) => {
                    let mut state = self.state.borrow_mut();
                    state.failure = Some(error);
                    state.completed = true;
                    return Ok(());
                }
                None => {
                    self.state.borrow_mut().completed = true;
                    return Ok(());
                }
            }
        }
    }
}

struct ConnectionOwner<'net, Tcp, Resolver>
where
    Tcp: TcpConnect,
    Resolver: Dns,
{
    client: ReqwlessHttpClient<'net, Tcp, Resolver>,
    origin: String,
}

#[self_referencing]
struct Connected<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    owner: ConnectionOwner<'net, Tcp, Resolver>,
    #[borrows(mut owner)]
    #[not_covariant]
    resource: HttpResource<'this, Tcp::Connection<'this>>,
}

struct Transport<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    disconnected: Option<ReqwlessHttpClient<'net, Tcp, Resolver>>,
    connected: Option<Connected<'net, Tcp, Resolver>>,
    connected_origin: Option<String>,
    connection_healthy: bool,
    header_buffer: Vec<u8>,
    read_buffer: Vec<u8>,
    tls_configured: bool,
}

impl<'net, Tcp, Resolver> Transport<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    fn from_client(
        client: ReqwlessHttpClient<'net, Tcp, Resolver>,
        header_buffer_size: usize,
        read_buffer_size: usize,
        tls_configured: bool,
    ) -> Self {
        Self {
            disconnected: Some(client),
            connected: None,
            connected_origin: None,
            connection_healthy: true,
            header_buffer: vec![0; header_buffer_size],
            read_buffer: vec![0; read_buffer_size],
            tls_configured,
        }
    }

    fn disconnect(&mut self) {
        if let Some(connected) = self.connected.take() {
            self.disconnected = Some(connected.into_heads().owner.client);
        }
        self.connected_origin = None;
        self.connection_healthy = true;
    }

    async fn ensure_connected(&mut self, origin: &str) -> Result<(), Error> {
        if !self.connection_healthy {
            self.disconnect();
        }
        if self.connected_origin.as_deref() == Some(origin) && self.connected.is_some() {
            return Ok(());
        }
        self.disconnect();
        let client = self.disconnected.take().ok_or(Error::ConnectionAborted)?;
        let owner = ConnectionOwner {
            client,
            origin: origin.to_string(),
        };
        let builder = ConnectedAsyncTryBuilder {
            owner,
            resource_builder: |owner| {
                Box::pin(async move {
                    let ConnectionOwner { client, origin } = owner;
                    client.resource(origin.as_str()).await
                })
            },
        };
        match builder.try_build_or_recover().await {
            Ok(connected) => {
                self.connected = Some(connected);
                self.connected_origin = Some(origin.to_string());
                Ok(())
            }
            Err((error, heads)) => {
                self.disconnected = Some(heads.owner.client);
                Err(error.into())
            }
        }
    }

    async fn execute_inner(&mut self, request: Request) -> Result<Response, Error> {
        if request.url.starts_with("https://") && !self.tls_configured {
            return Err(Error::TlsNotConfigured);
        }
        validate_headers(&request)?;
        let (origin, path) = split_url(&request.url)?;
        let origin = origin.to_string();
        let path = path.to_string();
        self.ensure_connected(&origin).await?;

        let header_refs: Vec<(&str, &str)> = request
            .headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
            .collect();
        let method = map_method(request.method);
        let Self {
            connected,
            connection_healthy,
            header_buffer,
            read_buffer,
            ..
        } = self;
        *connection_healthy = false;
        let connected = connected.as_mut().ok_or(Error::ConnectionAborted)?;

        let result = connected
            .with_resource_mut(|resource| {
                Box::pin(async move {
                    match request.body {
                        Body::Empty => {
                            let response = resource
                                .request(method, &path)
                                .headers(&header_refs)
                                .send(header_buffer.as_mut_slice())
                                .await?;
                            read_response(response, read_buffer).await
                        }
                        Body::Bytes(bytes) => {
                            let response = resource
                                .request(method, &path)
                                .headers(&header_refs)
                                .body(bytes.as_slice())
                                .send(header_buffer.as_mut_slice())
                                .await?;
                            read_response(response, read_buffer).await
                        }
                        Body::Stream {
                            stream,
                            content_length,
                        } => {
                            let body = StreamBody::new(stream, content_length);
                            let probe = body.clone();
                            let response = resource
                                .request(method, &path)
                                .headers(&header_refs)
                                .body(body)
                                .send(header_buffer.as_mut_slice())
                                .await?;
                            if let Some(error) = probe.failure() {
                                return Err(Error::Body(error));
                            }
                            read_response(response, read_buffer).await
                        }
                    }
                }) as Pin<Box<dyn Future<Output = Result<Response, Error>> + '_>>
            })
            .await;
        if result.is_ok() {
            *connection_healthy = true;
        }
        result
    }

    async fn execute(&mut self, request: Request) -> Result<Response, Error> {
        let result = self.execute_inner(request).await;
        if result.is_err() {
            self.disconnect();
        }
        result
    }

    fn execute_stream(&mut self, request: Request) -> ResponseStream<'_> {
        try_yield_stream(|yielder| async move {
            if request.url.starts_with("https://") && !self.tls_configured {
                return Err(Error::TlsNotConfigured);
            }
            validate_headers(&request)?;
            let (origin, path) = split_url(&request.url)?;
            let origin = origin.to_string();
            let path = path.to_string();
            self.ensure_connected(&origin).await?;

            let header_refs = request
                .headers
                .iter()
                .map(|header| (header.name.as_str(), header.value.as_str()))
                .collect::<Vec<_>>();
            let method = map_method(request.method);
            let Self {
                connected,
                connection_healthy,
                header_buffer,
                read_buffer,
                ..
            } = self;
            *connection_healthy = false;
            let connected = connected.as_mut().ok_or(Error::ConnectionAborted)?;
            let result = connected
                .with_resource_mut(|resource| {
                    Box::pin(async move {
                        match request.body {
                            Body::Empty => {
                                let response = resource
                                    .request(method, &path)
                                    .headers(&header_refs)
                                    .send(header_buffer.as_mut_slice())
                                    .await?;
                                stream_response(response, read_buffer, &yielder).await
                            }
                            Body::Bytes(bytes) => {
                                let response = resource
                                    .request(method, &path)
                                    .headers(&header_refs)
                                    .body(bytes.as_slice())
                                    .send(header_buffer.as_mut_slice())
                                    .await?;
                                stream_response(response, read_buffer, &yielder).await
                            }
                            Body::Stream {
                                stream,
                                content_length,
                            } => {
                                let body = StreamBody::new(stream, content_length);
                                let probe = body.clone();
                                let response = resource
                                    .request(method, &path)
                                    .headers(&header_refs)
                                    .body(body)
                                    .send(header_buffer.as_mut_slice())
                                    .await?;
                                if let Some(error) = probe.failure() {
                                    return Err(Error::Body(error));
                                }
                                stream_response(response, read_buffer, &yielder).await
                            }
                        }
                    }) as Pin<Box<dyn Future<Output = Result<(), Error>> + '_>>
                })
                .await;
            if result.is_ok() {
                *connection_healthy = true;
            } else {
                self.disconnect();
            }
            result
        })
    }
}

async fn stream_response<C>(
    response: reqwless::response::Response<'_, '_, C>,
    read_buffer: &mut Vec<u8>,
    yielder: &barracuda_runtime_utils::yield_stream::Yielder<ResponsePart>,
) -> Result<(), Error>
where
    C: embedded_io_async::Read,
{
    yielder
        .yield_one(ResponsePart::Head(response.status.0))
        .await;
    let mut reader = response.body().reader();
    loop {
        let read = reader
            .read(read_buffer.as_mut_slice())
            .await
            .map_err(|error| reqwless::Error::Network(error.kind()))?;
        if read == 0 {
            return Ok(());
        }
        let chunk = read_buffer.get(..read).ok_or(reqwless::Error::Codec)?;
        yielder.yield_one(ResponsePart::Data(chunk.to_vec())).await;
    }
}

async fn read_response<C>(
    response: reqwless::response::Response<'_, '_, C>,
    read_buffer: &mut Vec<u8>,
) -> Result<Response, Error>
where
    C: embedded_io_async::Read,
{
    let status = response.status.0;
    let mut reader = response.body().reader();
    let mut body = Vec::new();
    loop {
        let read = reader
            .read(read_buffer.as_mut_slice())
            .await
            .map_err(|error| reqwless::Error::Network(error.kind()))?;
        if read == 0 {
            break;
        }
        let chunk = read_buffer.get(..read).ok_or(reqwless::Error::Codec)?;
        body.extend_from_slice(chunk);
    }
    Ok(Response { status, body })
}

fn map_method(method: Method) -> ReqwlessMethod {
    match method {
        Method::Get => ReqwlessMethod::GET,
        Method::Post => ReqwlessMethod::POST,
        Method::Put => ReqwlessMethod::PUT,
        Method::Delete => ReqwlessMethod::DELETE,
        Method::Patch => ReqwlessMethod::PATCH,
    }
}

fn validate_headers(request: &Request) -> Result<(), Error> {
    if request.headers.iter().any(|header| {
        header.name.is_empty()
            || header.name.bytes().any(|byte| byte <= b' ' || byte == b':')
            || header.value.contains('\r')
            || header.value.contains('\n')
    }) {
        return Err(Error::InvalidHeader);
    }
    Ok(())
}

fn split_url(url: &str) -> Result<(&str, &str), Error> {
    let scheme_end = url.find("://").ok_or(Error::InvalidUrl)?;
    let authority_start = scheme_end.checked_add(3).ok_or(Error::InvalidUrl)?;
    let scheme = url.get(..scheme_end).ok_or(Error::InvalidUrl)?;
    if !matches!(scheme, "http" | "https") {
        return Err(Error::InvalidUrl);
    }
    let path_start = url
        .get(authority_start..)
        .and_then(|remainder| remainder.find('/'))
        .and_then(|offset| authority_start.checked_add(offset));
    match path_start {
        Some(index) if index > authority_start => Ok((
            url.get(..index).ok_or(Error::InvalidUrl)?,
            url.get(index..).ok_or(Error::InvalidUrl)?,
        )),
        None if authority_start < url.len() => Ok((url, "/")),
        _ => Err(Error::InvalidUrl),
    }
}

pub struct ReqwlessClient<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    transport: Mutex<NoopRawMutex, Transport<'net, Tcp, Resolver>>,
}

/// Default storage reserved for encoded response headers.
pub const DEFAULT_HEADER_BUFFER_SIZE: usize = 16 * 1024;
/// Default chunk size used while reading response bodies.
pub const DEFAULT_READ_BUFFER_SIZE: usize = 8 * 1024;

impl<'net, Tcp, Resolver> ReqwlessClient<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    /// Creates a plaintext HTTP client with practical default buffer sizes.
    #[must_use]
    pub fn new(tcp: &'net Tcp, resolver: &'net Resolver) -> Self {
        Self::with_buffer_sizes(
            tcp,
            resolver,
            DEFAULT_HEADER_BUFFER_SIZE,
            DEFAULT_READ_BUFFER_SIZE,
        )
    }

    /// Creates a plaintext HTTP client with caller-selected buffer sizes.
    #[must_use]
    pub fn with_buffer_sizes(
        tcp: &'net Tcp,
        resolver: &'net Resolver,
        header_buffer_size: usize,
        read_buffer_size: usize,
    ) -> Self {
        Self {
            transport: Mutex::new(Transport::from_client(
                ReqwlessHttpClient::new(tcp, resolver),
                header_buffer_size,
                read_buffer_size,
                false,
            )),
        }
    }
}

#[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
impl<'net, Tcp, Resolver> ReqwlessClient<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    /// Creates an HTTPS client with caller-selected buffer sizes.
    #[must_use]
    pub fn with_tls_and_buffer_sizes(
        tcp: &'net Tcp,
        resolver: &'net Resolver,
        tls: reqwless::client::TlsConfig<'net>,
        header_buffer_size: usize,
        read_buffer_size: usize,
    ) -> Self {
        Self {
            transport: Mutex::new(Transport::from_client(
                ReqwlessHttpClient::new_with_tls(tcp, resolver, tls),
                header_buffer_size,
                read_buffer_size,
                true,
            )),
        }
    }
}

impl<'net, Tcp, Resolver> HttpClient for ReqwlessClient<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    fn execute(&self, request: Request) -> HttpFuture<'_> {
        Box::pin(async move { self.transport.lock().await.execute(request).await })
    }

    fn execute_stream(&self, request: Request) -> ResponseStream<'_> {
        try_yield_stream(|yielder| async move {
            let mut transport = self.transport.lock().await;
            let mut stream = transport.execute_stream(request);
            while let Some(part) = futures_lite::StreamExt::next(&mut stream).await {
                yielder.yield_one(part?).await;
            }
            Ok(())
        })
    }
}

//! Persistent reqwless transport owned by [`crate::ModelApi`].

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;

use barracuda_runtime_utils::yield_stream::try_yield_stream;
use embedded_io_async::Read as _;
use embedded_nal_async::{Dns, TcpConnect};
use futures_core::Stream;
use ouroboros::self_referencing;
use reqwless::client::{HttpClient, HttpResource};
use reqwless::headers::ContentType;
use reqwless::request::RequestBuilder as _;
use reqwless::response::StatusCode;

#[cfg(all(feature = "embedded-tls", feature = "mbedtls"))]
compile_error!("select exactly one barracuda-model-api TLS backend");

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) status: StatusCode,
    pub(crate) body: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResponsePart {
    Head(StatusCode),
    Data(Vec<u8>),
}

pub(crate) type ResponseStream<'a> = Pin<Box<dyn Stream<Item = Result<ResponsePart, Error>> + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("request was cancelled")]
    Cancelled,
    #[error("HTTPS requested without a TLS configuration")]
    TlsNotConfigured,
    #[error("invalid URL")]
    InvalidUrl,
    #[error("HTTP codec error")]
    Codec,
    #[error("connection was aborted")]
    ConnectionAborted,
    #[error("response body is not UTF-8")]
    InvalidUtf8,
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

struct ConnectionOwner<'net, Tcp, Resolver>
where
    Tcp: TcpConnect,
    Resolver: Dns,
{
    client: HttpClient<'net, Tcp, Resolver>,
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

/// One reqwless client and, while connected, its persistent HTTP resource.
pub(crate) struct HttpTransport<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    disconnected: Option<HttpClient<'net, Tcp, Resolver>>,
    connected: Option<Connected<'net, Tcp, Resolver>>,
    connected_origin: Option<String>,
    connection_healthy: Cell<bool>,
    header_buffer: Vec<u8>,
    read_buffer: Vec<u8>,
    tls_configured: bool,
}

impl<'net, Tcp, Resolver> HttpTransport<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    #[must_use]
    pub(crate) fn new(
        tcp: &'net Tcp,
        resolver: &'net Resolver,
        header_buffer_size: usize,
        read_buffer_size: usize,
    ) -> Self {
        Self::from_client(
            HttpClient::new(tcp, resolver),
            header_buffer_size,
            read_buffer_size,
            false,
        )
    }

    fn from_client(
        client: HttpClient<'net, Tcp, Resolver>,
        header_buffer_size: usize,
        read_buffer_size: usize,
        tls_configured: bool,
    ) -> Self {
        Self {
            disconnected: Some(client),
            connected: None,
            connected_origin: None,
            connection_healthy: Cell::new(true),
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
        self.connection_healthy.set(true);
    }

    async fn ensure_connected(&mut self, origin: &str) -> Result<(), Error> {
        if !self.connection_healthy.get() {
            self.disconnect();
        }
        if self.connected_origin.as_deref() == Some(origin) && self.connected.is_some() {
            return Ok(());
        }
        self.disconnect();
        let client = self.disconnected.take().ok_or(Error::ConnectionAborted)?;
        let owner = ConnectionOwner {
            client,
            origin: origin.into(),
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
                self.connected_origin = Some(origin.into());
                Ok(())
            }
            Err((error, heads)) => {
                self.disconnected = Some(heads.owner.client);
                Err(error.into())
            }
        }
    }

    async fn post_json_inner(
        &mut self,
        url: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        if url.starts_with("https://") && !self.tls_configured {
            return Err(Error::TlsNotConfigured);
        }
        let (origin, path) = split_url(url)?;
        self.ensure_connected(origin).await?;
        let Self {
            connected,
            header_buffer,
            read_buffer,
            connection_healthy,
            ..
        } = self;
        connection_healthy.set(false);
        let connected = connected.as_mut().ok_or(Error::ConnectionAborted)?;
        let result = connected
            .with_resource_mut(|resource| {
                Box::pin(async move {
                    let response = resource
                        .post(path)
                        .headers(headers)
                        .content_type(ContentType::ApplicationJson)
                        .body(body.as_bytes())
                        .send(header_buffer.as_mut_slice())
                        .await?;
                    let status = response.status;
                    let mut reader = response.body().reader();
                    let mut response_body = Vec::new();
                    loop {
                        let read = reader.read(read_buffer.as_mut_slice()).await?;
                        if read == 0 {
                            break;
                        }
                        let chunk = read_buffer.get(..read).ok_or(Error::Codec)?;
                        response_body.extend_from_slice(chunk);
                    }
                    let body = String::from_utf8(response_body).map_err(|_| Error::InvalidUtf8)?;
                    Ok(Response { status, body })
                }) as Pin<Box<dyn Future<Output = Result<Response, Error>> + '_>>
            })
            .await;
        if result.is_ok() {
            connection_healthy.set(true);
        }
        result
    }

    pub(crate) async fn post_json(
        &mut self,
        url: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        let result = self.post_json_inner(url, body, headers).await;
        if result.is_err() {
            self.disconnect();
        }
        result
    }

    pub(crate) fn post_json_stream<'a>(
        &'a mut self,
        url: String,
        body: String,
        headers: Vec<(String, String)>,
    ) -> ResponseStream<'a> {
        try_yield_stream(|yielder| async move {
            if url.starts_with("https://") && !self.tls_configured {
                return Err(Error::TlsNotConfigured);
            }
            let (origin, path) = split_url(&url)?;
            self.ensure_connected(origin).await?;
            let header_refs: Vec<(&str, &str)> = headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let result = {
                let Self {
                    connected,
                    header_buffer,
                    read_buffer,
                    connection_healthy,
                    ..
                } = self;
                connection_healthy.set(false);
                let connected = connected.as_mut().ok_or(Error::ConnectionAborted)?;
                let result = connected
                    .with_resource_mut(|resource| {
                        Box::pin(async move {
                            let response = resource
                                .post(path)
                                .headers(&header_refs)
                                .content_type(ContentType::ApplicationJson)
                                .body(body.as_bytes())
                                .send(header_buffer.as_mut_slice())
                                .await?;
                            yielder.yield_one(ResponsePart::Head(response.status)).await;
                            let mut reader = response.body().reader();
                            loop {
                                let read = reader.read(read_buffer.as_mut_slice()).await?;
                                if read == 0 {
                                    return Ok(());
                                }
                                let chunk = read_buffer.get(..read).ok_or(Error::Codec)?;
                                yielder.yield_one(ResponsePart::Data(chunk.to_vec())).await;
                            }
                        })
                            as Pin<Box<dyn Future<Output = Result<(), Error>> + '_>>
                    })
                    .await;
                if result.is_ok() {
                    connection_healthy.set(true);
                }
                result
            };
            if result.is_err() {
                self.disconnect();
            }
            result
        })
    }
}

#[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
impl<'net, Tcp, Resolver> HttpTransport<'net, Tcp, Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    #[must_use]
    pub(crate) fn new_with_tls(
        tcp: &'net Tcp,
        resolver: &'net Resolver,
        tls: reqwless::client::TlsConfig<'net>,
        header_buffer_size: usize,
        read_buffer_size: usize,
    ) -> Self {
        Self::from_client(
            HttpClient::new_with_tls(tcp, resolver, tls),
            header_buffer_size,
            read_buffer_size,
            true,
        )
    }
}

fn split_url(url: &str) -> Result<(&str, &str), Error> {
    let scheme_end = url.find("://").ok_or(Error::InvalidUrl)?;
    let authority_start = scheme_end.saturating_add(3);
    let path_start = url
        .get(authority_start..)
        .and_then(|remainder| remainder.find('/'))
        .map(|offset| authority_start.saturating_add(offset));
    match path_start {
        Some(index) => Ok((
            url.get(..index).ok_or(Error::InvalidUrl)?,
            url.get(index..).ok_or(Error::InvalidUrl)?,
        )),
        None if authority_start < url.len() => Ok((url, "/")),
        None => Err(Error::InvalidUrl),
    }
}

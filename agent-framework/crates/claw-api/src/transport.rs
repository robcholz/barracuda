//! Persistent reqwless transport owned by [`crate::ClawApi`].

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use core::fmt;
use core::future::{poll_fn, Future};
use core::pin::Pin;
use core::task::Poll;

use claw_utils::yield_stream::try_yield_stream;
use claw_utils::Cancel;
use embedded_io_async::Read as _;
use embedded_nal_async::{Dns, TcpConnect};
use futures_core::Stream;
use ouroboros::self_referencing;
use reqwless::client::{HttpClient, HttpResource};
use reqwless::headers::ContentType;
use reqwless::request::RequestBuilder as _;

#[cfg(all(feature = "embedded-tls", feature = "mbedtls"))]
compile_error!("select exactly one claw-api TLS backend");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusCode(u16);

impl StatusCode {
    pub const OK: Self = Self(200);
    pub const NO_CONTENT: Self = Self(204);

    #[must_use]
    pub const fn new(code: u16) -> Self {
        Self(code)
    }

    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        self.0 >= 200 && self.0 < 300
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("request was cancelled")]
    Cancelled,
    #[error("HTTPS requested without a TLS configuration")]
    TlsNotConfigured,
    #[error("DNS lookup failed")]
    Dns,
    #[error("network error: {0:?}")]
    Network(embedded_io::ErrorKind),
    #[error("invalid URL")]
    InvalidUrl,
    #[error("TLS handshake or verification failed")]
    Tls,
    #[error("HTTP codec error")]
    Codec,
    #[error("HTTP buffer is too small")]
    BufferTooSmall,
    #[error("connection was aborted")]
    ConnectionAborted,
    #[error("response body is not UTF-8")]
    InvalidUtf8,
}

struct ConnectionOwner<'net, S>
where
    S: TcpConnect + Dns,
{
    client: HttpClient<'net, S, S>,
    origin: String,
}

#[self_referencing]
struct Connected<'net, S>
where
    S: TcpConnect + Dns + 'net,
{
    owner: ConnectionOwner<'net, S>,
    #[borrows(mut owner)]
    #[not_covariant]
    resource: HttpResource<'this, S::Connection<'this>>,
}

/// One reqwless client and, while connected, its persistent HTTP resource.
pub(crate) struct HttpTransport<'net, S>
where
    S: TcpConnect + Dns + 'net,
{
    disconnected: Option<HttpClient<'net, S, S>>,
    connected: Option<Connected<'net, S>>,
    connected_origin: Option<String>,
    connection_healthy: Cell<bool>,
    header_buffer: Vec<u8>,
    read_buffer: Vec<u8>,
    tls_configured: bool,
}

impl<'net, S> HttpTransport<'net, S>
where
    S: TcpConnect + Dns + 'net,
{
    #[must_use]
    pub(crate) fn new(
        network: &'net S,
        header_buffer_size: usize,
        read_buffer_size: usize,
    ) -> Self {
        Self {
            disconnected: Some(HttpClient::new(network, network)),
            connected: None,
            connected_origin: None,
            connection_healthy: Cell::new(true),
            header_buffer: vec![0; header_buffer_size],
            read_buffer: vec![0; read_buffer_size],
            tls_configured: false,
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
                Err(map_reqwless_error(error))
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
                        .await
                        .map_err(map_reqwless_error)?;
                    let status = StatusCode::new(response.status.0);
                    let mut reader = response.body().reader();
                    let mut response_body = Vec::new();
                    loop {
                        let read = reader
                            .read(read_buffer.as_mut_slice())
                            .await
                            .map_err(map_reqwless_error)?;
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
        cancel: Cancel<'_>,
    ) -> Result<Response, Error> {
        let result = {
            let mut transfer = core::pin::pin!(self.post_json_inner(url, body, headers));
            poll_fn(move |context| {
                if cancel.is_cancelled() {
                    Poll::Ready(Err(Error::Cancelled))
                } else {
                    transfer.as_mut().poll(context)
                }
            })
            .await
        };
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
        cancel: Cancel<'a>,
    ) -> ResponseStream<'a> {
        try_yield_stream(|yielder| async move {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
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
                                .await
                                .map_err(map_reqwless_error)?;
                            yielder
                                .yield_one(ResponsePart::Head(StatusCode::new(response.status.0)))
                                .await;
                            let mut reader = response.body().reader();
                            loop {
                                if cancel.is_cancelled() {
                                    return Err(Error::Cancelled);
                                }
                                let read = reader
                                    .read(read_buffer.as_mut_slice())
                                    .await
                                    .map_err(map_reqwless_error)?;
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

#[cfg(feature = "embedded-tls")]
impl<'net, S> HttpTransport<'net, S>
where
    S: TcpConnect + Dns + 'net,
{
    #[must_use]
    pub(crate) fn new_with_tls(
        network: &'net S,
        tls: reqwless::client::TlsConfig<'net>,
        header_buffer_size: usize,
        read_buffer_size: usize,
    ) -> Self {
        Self {
            disconnected: Some(HttpClient::new_with_tls(network, network, tls)),
            connected: None,
            connected_origin: None,
            connection_healthy: Cell::new(true),
            header_buffer: vec![0; header_buffer_size],
            read_buffer: vec![0; read_buffer_size],
            tls_configured: true,
        }
    }
}

#[cfg(feature = "mbedtls")]
impl<'net, S> HttpTransport<'net, S>
where
    S: TcpConnect + Dns + 'net,
{
    #[must_use]
    pub(crate) fn new_with_tls(
        network: &'net S,
        tls: reqwless::client::TlsConfig<'net>,
        header_buffer_size: usize,
        read_buffer_size: usize,
    ) -> Self {
        Self {
            disconnected: Some(HttpClient::new_with_tls(network, network, tls)),
            connected: None,
            connected_origin: None,
            connection_healthy: Cell::new(true),
            header_buffer: vec![0; header_buffer_size],
            read_buffer: vec![0; read_buffer_size],
            tls_configured: true,
        }
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

fn map_reqwless_error(error: reqwless::Error) -> Error {
    match error {
        reqwless::Error::Dns => Error::Dns,
        reqwless::Error::Network(kind) => Error::Network(kind),
        reqwless::Error::Codec
        | reqwless::Error::AlreadySent
        | reqwless::Error::IncorrectBodyWritten => Error::Codec,
        reqwless::Error::InvalidUrl(_) => Error::InvalidUrl,
        #[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
        reqwless::Error::Tls(_) => Error::Tls,
        reqwless::Error::BufferTooSmall => Error::BufferTooSmall,
        reqwless::Error::ConnectionAborted => Error::ConnectionAborted,
    }
}

//! Agent-owned persistent reqwless transport.

use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{future::Future, pin::Pin};

use barracuda_runtime_utils::yield_stream::try_yield_stream;
use embedded_io::Error as _;
use embedded_io_async::Read as _;
use embedded_nal_async::{Dns, TcpConnect};
use futures_core::Stream;
use ouroboros::self_referencing;
use reqwless::{
    client::{HttpClient, HttpResource},
    headers::ContentType,
    request::RequestBuilder as _,
};

use crate::Error;

const HEADER_BUFFER_SIZE: usize = 16 * 1024;
const READ_BUFFER_SIZE: usize = 8 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) body: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResponsePart {
    Head(u16),
    Data(Vec<u8>),
}

pub(crate) type ResponseStream<'a> = Pin<Box<dyn Stream<Item = Result<ResponsePart, Error>> + 'a>>;

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
pub(crate) struct Transport<'net, Tcp = http_client::Tcp, Resolver = http_client::Resolver>
where
    Tcp: TcpConnect + 'net,
    Resolver: Dns + 'net,
{
    disconnected: Option<HttpClient<'net, Tcp, Resolver>>,
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
    #[must_use]
    pub(crate) fn new(factory: http_client::ClientFactory<'net, Tcp, Resolver>) -> Self {
        let (client, tls_configured) = factory.create();
        Self {
            disconnected: Some(client),
            connected: None,
            connected_origin: None,
            connection_healthy: true,
            header_buffer: vec![0; HEADER_BUFFER_SIZE],
            read_buffer: vec![0; READ_BUFFER_SIZE],
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

    async fn post_json_inner(
        &mut self,
        url: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        if url.starts_with("https://") && !self.tls_configured {
            return Err(Error::TlsNotConfigured);
        }
        validate_headers(headers)?;
        let (origin, path) = split_url(url)?;
        let origin = origin.to_string();
        let path = path.to_string();
        self.ensure_connected(&origin).await?;
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
                    let response = resource
                        .post(&path)
                        .headers(headers)
                        .content_type(ContentType::ApplicationJson)
                        .body(body.as_bytes())
                        .send(header_buffer.as_mut_slice())
                        .await?;
                    let status = response.status.0;
                    let mut reader = response.body().reader();
                    let mut response_body = Vec::new();
                    loop {
                        let read = reader
                            .read(read_buffer.as_mut_slice())
                            .await
                            .map_err(|error| reqwless::Error::Network(error.kind()))?;
                        if read == 0 {
                            break;
                        }
                        let chunk = read_buffer.get(..read).ok_or(Error::HttpCodec)?;
                        response_body.extend_from_slice(chunk);
                    }
                    let body =
                        String::from_utf8(response_body).map_err(|_error| Error::InvalidUtf8)?;
                    Ok(Response { status, body })
                }) as Pin<Box<dyn Future<Output = Result<Response, Error>> + '_>>
            })
            .await;
        if result.is_ok() {
            *connection_healthy = true;
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
            let header_refs = headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect::<Vec<_>>();
            validate_headers(&header_refs)?;
            let (origin, path) = split_url(&url)?;
            let origin = origin.to_string();
            let path = path.to_string();
            self.ensure_connected(&origin).await?;
            let result = {
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
                            let response = resource
                                .post(&path)
                                .headers(&header_refs)
                                .content_type(ContentType::ApplicationJson)
                                .body(body.as_bytes())
                                .send(header_buffer.as_mut_slice())
                                .await?;
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
                                let chunk = read_buffer.get(..read).ok_or(Error::HttpCodec)?;
                                yielder.yield_one(ResponsePart::Data(chunk.to_vec())).await;
                            }
                        })
                            as Pin<Box<dyn Future<Output = Result<(), Error>> + '_>>
                    })
                    .await;
                if result.is_ok() {
                    *connection_healthy = true;
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

fn validate_headers(headers: &[(&str, &str)]) -> Result<(), Error> {
    if headers.iter().any(|(name, value)| {
        name.is_empty()
            || name.bytes().any(|byte| byte <= b' ' || byte == b':')
            || value.contains('\r')
            || value.contains('\n')
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

#[cfg(test)]
mod tests {
    use crate::Error;

    #[test]
    fn transport_errors_classify_retryability_directly() {
        let transient = Error::ConnectionAborted;
        assert!(transient.is_retryable());

        let permanent = Error::InvalidUrl;
        assert!(!permanent.is_retryable());
        assert!(!Error::Cancelled.is_retryable());
    }
}

use alloc::{boxed::Box, vec};
extern crate alloc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RpcFrame, RpcHandler, RpcMethod,
    RunContext, Unary, UnregisterContext, rpc_dynamic,
};
use barracuda_http_wire::{
    HttpMethod, HttpRequest, HttpResponse, HttpRpcError, HttpText, HttpTextError,
    RESPONSE_BODY_CAPACITY,
};
use http_client::{
    ClientFactory,
    embedded_nal_async::{Dns, TcpConnect},
    reqwless::request::{Method, RequestBuilder as _},
};

const HEADER_BUFFER_SIZE: usize = 16 * 1024;

struct Request;
#[rpc_dynamic]
impl RpcMethod for Request {
    const ADDRESS: &'static str = "http.request";
    type Request = HttpRequest;
    type Response = HttpResponse;
    type Error = HttpRpcError;
    type Input = Unary;
    type Output = Unary;
}

fn handler<T, D>(clients: ClientFactory<'static, T, D>) -> impl RpcHandler<Request>
where
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    move |_context, frame: RpcFrame<HttpRequest>| {
        let clients = clients.clone();
        async move { Ok(execute(&clients, frame.view()?).await) }
    }
}

fn request_text<const N: usize>(
    value: &HttpText<N>,
    error: HttpRpcError,
) -> Result<&str, HttpRpcError> {
    value.as_str().map_err(|_error| error)
}

fn request_headers(input: &HttpRequest) -> Result<alloc::vec::Vec<(&str, &str)>, HttpRpcError> {
    let mut headers = alloc::vec::Vec::new();
    for header in &input.headers {
        let name = request_text(&header.name, HttpRpcError::InvalidHeader)?;
        let value = request_text(&header.value, HttpRpcError::InvalidHeader)?;
        if name.is_empty() && value.is_empty() {
            continue;
        }
        if name.is_empty()
            || name.bytes().any(|byte| byte <= b' ' || byte == b':')
            || value.contains('\r')
            || value.contains('\n')
        {
            return Err(HttpRpcError::InvalidHeader);
        }
        headers.push((name, value));
    }
    Ok(headers)
}

fn response_body(text: &str) -> Result<HttpText<{ RESPONSE_BODY_CAPACITY + 1 }>, HttpRpcError> {
    HttpText::new(text).map_err(|error| match error {
        HttpTextError::TooLong => HttpRpcError::ResponseTooLarge,
        HttpTextError::EmbeddedNul
        | HttpTextError::InvalidTerminator
        | HttpTextError::InvalidUtf8 => HttpRpcError::InvalidResponseText,
    })
}

async fn execute<T: TcpConnect, D: Dns>(
    clients: &ClientFactory<'_, T, D>,
    input: &HttpRequest,
) -> Result<HttpResponse, HttpRpcError> {
    let url = request_text(&input.url, HttpRpcError::InvalidUrl)?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(HttpRpcError::InvalidUrl);
    }
    let headers = request_headers(input)?;
    let body = request_text(&input.body, HttpRpcError::Transport)?;
    let (mut client, tls) = clients.create();
    if url.starts_with("https://") && !tls {
        return Err(HttpRpcError::TlsNotConfigured);
    }
    let method = match input.method {
        HttpMethod::Get => Method::GET,
        HttpMethod::Post => Method::POST,
        HttpMethod::Put => Method::PUT,
        HttpMethod::Patch => Method::PATCH,
        HttpMethod::Delete => Method::DELETE,
        HttpMethod::Head => Method::HEAD,
    };
    let mut header_buffer = vec![0; HEADER_BUFFER_SIZE];
    let request = client
        .request(method, url)
        .await
        .map_err(|_| HttpRpcError::Transport)?;
    let mut request = request.headers(&headers).body(body.as_bytes());
    let response = request
        .send(&mut header_buffer)
        .await
        .map_err(|_| HttpRpcError::Transport)?;
    let status = response.status.0;
    let mut reader = response.body().reader();
    let mut bytes = vec![0; RESPONSE_BODY_CAPACITY];
    let mut used = 0;
    loop {
        let read = embedded_io_async::Read::read(
            &mut reader,
            bytes
                .get_mut(used..)
                .ok_or(HttpRpcError::ResponseTooLarge)?,
        )
        .await
        .map_err(|_| HttpRpcError::Transport)?;
        if read == 0 {
            break;
        }
        used = used
            .checked_add(read)
            .ok_or(HttpRpcError::ResponseTooLarge)?;
        if used == RESPONSE_BODY_CAPACITY {
            return Err(HttpRpcError::ResponseTooLarge);
        }
    }
    let text = core::str::from_utf8(bytes.get(..used).ok_or(HttpRpcError::Transport)?)
        .map_err(|_| HttpRpcError::InvalidResponseText)?;
    Ok(HttpResponse::new(status, response_body(text)?))
}

/// Event Router Component serving the dynamic `http.request` RPC.
pub struct HttpComponent<T: 'static = http_client::Tcp, D: 'static = http_client::Resolver> {
    clients: ClientFactory<'static, T, D>,
}
impl<T, D> HttpComponent<T, D> {
    /// Creates a Component from System's shared HTTP client factory.
    #[must_use]
    pub fn new(clients: ClientFactory<'static, T, D>) -> Self {
        Self { clients }
    }
}
impl<T: TcpConnect + 'static, D: Dns + 'static, const M: usize> Component<M>
    for HttpComponent<T, D>
{
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<Request, _>(handler(self.clients.clone()))
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
    #![allow(clippy::panic)]
    #![allow(clippy::unwrap_used)]

    use core::{
        future::Future,
        net::{IpAddr, SocketAddr},
        pin::pin,
        task::{Context, Poll, Waker},
    };

    use barracuda_http_wire::HttpHeader;
    use embedded_io::{ErrorKind, ErrorType};
    use http_client::embedded_nal_async::AddrType;

    use super::*;

    struct UnusedNetwork;
    struct UnusedConnection;

    impl ErrorType for UnusedConnection {
        type Error = ErrorKind;
    }

    impl embedded_io_async::Read for UnusedConnection {
        async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
            Err(ErrorKind::Other)
        }
    }

    impl embedded_io_async::Write for UnusedConnection {
        async fn write(&mut self, _buf: &[u8]) -> Result<usize, Self::Error> {
            Err(ErrorKind::Other)
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Err(ErrorKind::Other)
        }
    }

    impl TcpConnect for UnusedNetwork {
        type Error = ErrorKind;
        type Connection<'a>
            = UnusedConnection
        where
            Self: 'a;

        async fn connect<'a>(
            &'a self,
            _remote: SocketAddr,
        ) -> Result<Self::Connection<'a>, Self::Error> {
            Err(ErrorKind::Other)
        }
    }

    impl Dns for UnusedNetwork {
        type Error = ErrorKind;

        async fn get_host_by_name(
            &self,
            _host: &str,
            _addr_type: AddrType,
        ) -> Result<IpAddr, Self::Error> {
            Err(ErrorKind::Other)
        }

        async fn get_host_by_address(
            &self,
            _addr: IpAddr,
            _result: &mut [u8],
        ) -> Result<usize, Self::Error> {
            Err(ErrorKind::Other)
        }
    }

    fn ready<T>(future: impl Future<Output = T>) -> T {
        let mut future = pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("future should complete without network I/O"),
        }
    }

    fn execute_now(input: &HttpRequest) -> Result<HttpResponse, HttpRpcError> {
        let tcp = UnusedNetwork;
        let dns = UnusedNetwork;
        let clients = ClientFactory::from_network(&tcp, &dns);
        ready(execute(&clients, input))
    }

    fn header(name: &str, value: &str) -> HttpHeader {
        HttpHeader {
            name: HttpText::new(name).unwrap(),
            value: HttpText::new(value).unwrap(),
        }
    }

    fn request_with_headers(headers: [HttpHeader; 2]) -> HttpRequest {
        HttpRequest {
            method: HttpMethod::Get,
            url: HttpText::new("http://example.com").unwrap(),
            headers,
            body: HttpText::default(),
        }
    }

    #[test]
    fn unused_header_slots_are_skipped() {
        let request = request_with_headers([HttpHeader::default(), header("Accept", "text/plain")]);
        let headers = request_headers(&request).unwrap();
        assert_eq!(headers.as_slice(), [("Accept", "text/plain")]);
    }

    #[test]
    fn empty_header_name_with_value_is_invalid() {
        let request = request_with_headers([header("", "present"), HttpHeader::default()]);
        assert_eq!(request_headers(&request), Err(HttpRpcError::InvalidHeader));
        assert_eq!(execute_now(&request), Err(HttpRpcError::InvalidHeader));
    }

    #[test]
    fn header_name_with_space_or_colon_is_invalid() {
        assert_eq!(
            request_headers(&request_with_headers([
                header("X Header", "1"),
                HttpHeader::default(),
            ])),
            Err(HttpRpcError::InvalidHeader)
        );
        assert_eq!(
            request_headers(&request_with_headers([
                header("X:Name", "1"),
                HttpHeader::default(),
            ])),
            Err(HttpRpcError::InvalidHeader)
        );
    }

    #[test]
    fn header_value_with_crlf_is_invalid() {
        let request = request_with_headers([header("X-Test", "a\r\nb"), HttpHeader::default()]);
        assert_eq!(request_headers(&request), Err(HttpRpcError::InvalidHeader));
        assert_eq!(execute_now(&request), Err(HttpRpcError::InvalidHeader));
    }
}

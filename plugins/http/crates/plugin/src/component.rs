use alloc::{boxed::Box, vec};
extern crate alloc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RpcFrame, RpcHandler, RpcMethod,
    RunContext, Unary, UnregisterContext, rpc_dynamic,
};
use barracuda_http_wire::{
    HttpMethod, HttpRequest, HttpResponse, HttpRpcError, HttpText, RESPONSE_BODY_CAPACITY,
};
use http_client::{
    ClientFactory,
    embedded_nal_async::{Dns, TcpConnect},
};
use reqwless::request::{Method, RequestBuilder as _};

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

async fn execute<T: TcpConnect, D: Dns>(
    clients: &ClientFactory<'_, T, D>,
    input: &HttpRequest,
) -> Result<HttpResponse, HttpRpcError> {
    let url = input.url.as_str();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(HttpRpcError::InvalidUrl);
    }
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
    let headers: alloc::vec::Vec<(&str, &str)> = input
        .headers
        .iter()
        .filter(|header| !header.name.as_str().is_empty())
        .map(|header| (header.name.as_str(), header.value.as_str()))
        .collect();
    let mut header_buffer = vec![0; HEADER_BUFFER_SIZE];
    let request = client
        .request(method, url)
        .await
        .map_err(|_| HttpRpcError::Transport)?;
    let mut request = request
        .headers(&headers)
        .body(input.body.as_str().as_bytes());
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
    let body = HttpText::new(text).map_err(|_| HttpRpcError::ResponseTooLarge)?;
    Ok(HttpResponse::new(status, body))
}

pub struct HttpComponent<T: 'static = http_client::Tcp, D: 'static = http_client::Resolver> {
    clients: ClientFactory<'static, T, D>,
}
impl<T, D> HttpComponent<T, D> {
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

use alloc::{boxed::Box, format, rc::Rc, string::String, vec, vec::Vec};
use core::{cell::RefCell, future::pending};

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RpcFrame, RpcHandler, RpcMethod,
    RpcStream, RunContext, Streaming, Unary, UnregisterContext, rpc_dynamic,
};
use barracuda_web_search_wire::{
    CONTENT_CAPACITY, TITLE_CAPACITY, URL_CAPACITY, WebSearchError, WebSearchRequest,
    WebSearchResult,
};
use futures_lite::stream;
use http_client::ClientFactory;
use http_client::reqwless::request::RequestBuilder as _;
use serde::{Deserialize, Serialize};

const HEADER_BUFFER_SIZE: usize = 8 * 1024;
const READ_BUFFER_SIZE: usize = 4 * 1024;
const MAX_RESPONSE_SIZE: usize = 64 * 1024;

#[derive(Clone)]
pub(crate) struct TavilyConfig {
    pub(crate) api_key: String,
    pub(crate) api_base: String,
}

/// Searches the public web through Tavily.
pub struct WebSearch;

#[rpc_dynamic]
impl RpcMethod for WebSearch {
    const ADDRESS: &'static str = "web_search.search";
    type Request = WebSearchRequest;
    type Response = WebSearchResult;
    type Error = WebSearchError;
    type Input = Unary;
    type Output = Streaming;
}

/// Event Router Component serving the Agent-facing web-search RPC.
pub struct WebSearchComponent {
    config: Rc<RefCell<Option<TavilyConfig>>>,
    http_clients: ClientFactory<'static>,
}

impl WebSearchComponent {
    pub(crate) fn new(
        config: Rc<RefCell<Option<TavilyConfig>>>,
        http_clients: ClientFactory<'static>,
    ) -> Self {
        Self {
            config,
            http_clients,
        }
    }
}

impl<const M: usize> Component<M> for WebSearchComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<WebSearch, _>(search_handler(
            Rc::clone(&self.config),
            self.http_clients.clone(),
        ))
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

fn search_handler(
    config: Rc<RefCell<Option<TavilyConfig>>>,
    http_clients: ClientFactory<'static>,
) -> impl RpcHandler<WebSearch> {
    move |_context, frame: RpcFrame<WebSearchRequest>| {
        let config = config.borrow().clone();
        let http_clients = http_clients.clone();
        async move {
            let outcome = match config {
                Some(config) => search(&http_clients, &config, &frame).await,
                None => Err(WebSearchError::NotConfigured),
            };
            let items = match outcome {
                Ok(results) => results.into_iter().map(|result| Ok(Ok(result))).collect(),
                Err(error) => vec![Ok(Err(error))],
            };
            Ok(RpcStream::new(stream::iter(items)))
        }
    }
}

#[derive(Serialize)]
struct ApiRequest<'a> {
    api_key: &'a str,
    query: &'a str,
    search_depth: &'static str,
    max_results: u8,
}

#[derive(Deserialize)]
struct ApiResponse {
    results: Vec<ApiResult>,
}

#[derive(Deserialize)]
struct ApiResult {
    title: String,
    url: String,
    content: String,
    score: f32,
}

async fn search(
    http_clients: &ClientFactory<'static>,
    config: &TavilyConfig,
    frame: &RpcFrame<WebSearchRequest>,
) -> Result<Vec<WebSearchResult>, WebSearchError> {
    let request = frame
        .view()
        .map_err(|_error| WebSearchError::InvalidRequest)?;
    let query = request
        .query()
        .map_err(|_error| WebSearchError::InvalidRequest)?;
    if query.trim().is_empty() || !(1..=10).contains(&request.max_results) {
        return Err(WebSearchError::InvalidRequest);
    }
    let body = serde_json::to_vec(&ApiRequest {
        api_key: &config.api_key,
        query,
        search_depth: "advanced",
        max_results: request.max_results,
    })
    .map_err(|_error| WebSearchError::InvalidRequest)?;
    let url = format!("{}/search", config.api_base.trim_end_matches('/'));
    let (mut client, tls_configured) = http_clients.create();
    if url.starts_with("https://") && !tls_configured {
        return Err(WebSearchError::Transport);
    }
    let request = client
        .request(http_client::reqwless::request::Method::POST, &url)
        .await
        .map_err(|_error| WebSearchError::Transport)?;
    let mut header_buffer = vec![0; HEADER_BUFFER_SIZE];
    let mut request = request
        .content_type(http_client::reqwless::headers::ContentType::ApplicationJson)
        .body(body.as_slice());
    let response = request
        .send(&mut header_buffer)
        .await
        .map_err(|_error| WebSearchError::Transport)?;
    if !(200..300).contains(&response.status.0) {
        log::warn!("Tavily search returned HTTP {}", response.status.0);
        return Err(WebSearchError::Service);
    }
    let mut reader = response.body().reader();
    let mut read_buffer = vec![0; READ_BUFFER_SIZE];
    let mut bytes = Vec::new();
    loop {
        let read = embedded_io_async::Read::read(&mut reader, &mut read_buffer)
            .await
            .map_err(|_error| WebSearchError::Transport)?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > MAX_RESPONSE_SIZE {
            return Err(WebSearchError::InvalidResponse);
        }
        bytes.extend_from_slice(
            read_buffer
                .get(..read)
                .ok_or(WebSearchError::InvalidResponse)?,
        );
    }
    let response: ApiResponse =
        serde_json::from_slice(&bytes).map_err(|_error| WebSearchError::InvalidResponse)?;
    response
        .results
        .into_iter()
        .map(|result| {
            WebSearchResult::new(
                truncate(&result.title, TITLE_CAPACITY - 1),
                truncate(&result.url, URL_CAPACITY - 1),
                truncate(&result.content, CONTENT_CAPACITY - 1),
                result.score,
            )
            .map_err(|_error| WebSearchError::InvalidResponse)
        })
        .collect()
}

fn truncate(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.get(..end).unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use barracuda_event_router::{RpcLaneStorage, RpcRegistry};
    use barracuda_platform_test::{loopback_network, never_embassy_stack};
    use core::time::Duration;
    use embassy_net::{Stack, tcp::TcpSocket};
    use embedded_io_async::Write as _;
    use futures_lite::future::zip;

    const PORT: u16 = 8789;

    fn registry(
        config: Option<TavilyConfig>,
        http_clients: ClientFactory<'static>,
    ) -> RpcRegistry<4, 1024, 4> {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 1024, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register::<WebSearch, _>(search_handler(Rc::new(RefCell::new(config)), http_clients))
            .expect("register web search endpoint");
        registry
    }

    async fn search_results(
        registry: &RpcRegistry<4, 1024, 4>,
        request: WebSearchRequest,
    ) -> Result<Vec<WebSearchResult>, WebSearchError> {
        let mut responses = registry
            .client()
            .call::<WebSearch>(request)
            .expect("start web search RPC");
        let mut results = Vec::new();
        while let Some(item) = responses.next().await {
            match item.expect("RPC transport succeeds") {
                Ok(frame) => results.push(*frame.view().expect("result frame is valid")),
                Err(frame) => return Err(*frame.view().expect("error frame is valid")),
            }
        }
        Ok(results)
    }

    fn http_response(status: u16, body: &[u8]) -> Vec<u8> {
        let reason = if (200..300).contains(&status) {
            "OK"
        } else {
            "Error"
        };
        let mut response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn request_is_complete(request: &[u8]) -> bool {
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        request.len() >= header_end + 4 + content_length
    }

    async fn serve_responses(
        stack: Stack<'static>,
        responses: Vec<Vec<u8>>,
        requests: Rc<RefCell<Vec<String>>>,
    ) {
        for response in responses {
            let mut tcp_rx = [0_u8; 8192];
            let mut tcp_tx = [0_u8; 4096];
            let mut socket = TcpSocket::new(stack, &mut tcp_rx, &mut tcp_tx);
            socket.accept(PORT).await.expect("accept provider request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 2048];
            while !request_is_complete(&request) {
                let length = socket
                    .read(&mut chunk)
                    .await
                    .expect("read provider request");
                assert_ne!(length, 0, "provider request closed before its body");
                request.extend_from_slice(&chunk[..length]);
            }
            requests
                .borrow_mut()
                .push(String::from_utf8(request).expect("HTTP request is UTF-8"));
            socket
                .write_all(&response)
                .await
                .expect("write provider response");
            socket.flush().await.expect("flush provider response");
        }
    }

    async fn search_with_provider_response(
        status: u16,
        body: Vec<u8>,
    ) -> Result<Vec<WebSearchResult>, WebSearchError> {
        let network = loopback_network();
        let stack = network.stack();
        let requests = Rc::new(RefCell::new(Vec::new()));
        let exchange = async {
            let server = serve_responses(
                stack,
                vec![http_response(status, &body)],
                Rc::clone(&requests),
            );
            let client = async {
                let registry = registry(
                    Some(TavilyConfig {
                        api_key: "secret".into(),
                        api_base: format!("http://10.0.0.1:{PORT}"),
                    }),
                    ClientFactory::plaintext(stack),
                );
                search_results(&registry, WebSearchRequest::new("query", 1).unwrap()).await
            };
            let (_, result) = zip(server, client).await;
            result
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                () = network.run() => panic!("network runner stopped"),
                result = exchange => result,
            }
        })
        .await
        .expect("provider exchange completes")
    }

    #[test]
    fn truncation_returns_short_values_unchanged() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("éclair", 1), "");
        assert_eq!(truncate("éclair", 2), "é");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_rpc_posts_provider_contract_and_bounds_utf8_results() {
        let title = format!("{}é-tail", "a".repeat(TITLE_CAPACITY - 2));
        let url = format!("http://example.test/{}", "u".repeat(URL_CAPACITY));
        let content = "内容".repeat(CONTENT_CAPACITY);
        let response = serde_json::to_vec(&serde_json::json!({
            "results": [
                {"title": title, "url": url, "content": content, "score": 0.75},
                {"title": "second", "url": "http://two.test", "content": "brief", "score": 0.5}
            ]
        }))
        .unwrap();
        let network = loopback_network();
        let stack = network.stack();
        let requests = Rc::new(RefCell::new(Vec::new()));
        let exchange = async {
            let server = serve_responses(
                stack,
                vec![http_response(200, &response)],
                Rc::clone(&requests),
            );
            let client = async {
                let registry = registry(
                    Some(TavilyConfig {
                        api_key: "secret".into(),
                        api_base: format!("http://10.0.0.1:{PORT}/api/"),
                    }),
                    ClientFactory::plaintext(stack),
                );
                search_results(
                    &registry,
                    WebSearchRequest::new("rust firmware", 2).unwrap(),
                )
                .await
            };
            let (_, results) = zip(server, client).await;
            results
        };
        let results = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                () = network.run() => panic!("network runner stopped"),
                results = exchange => results,
            }
        })
        .await
        .expect("provider exchange completes")
        .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title().unwrap(), "a".repeat(TITLE_CAPACITY - 2));
        assert!(results[0].url().unwrap().len() < URL_CAPACITY);
        assert!(results[0].content().unwrap().len() < CONTENT_CAPACITY);
        assert_eq!(results[0].score, 0.75);
        assert_eq!(results[1].title(), Ok("second"));
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("POST /api/search HTTP/1.1\r\n"));
        let body = requests[0].split_once("\r\n\r\n").unwrap().1;
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["api_key"], "secret");
        assert_eq!(body["query"], "rust firmware");
        assert_eq!(body["search_depth"], "advanced");
        assert_eq!(body["max_results"], 2);
    }

    #[test]
    fn search_rpc_preserves_distinct_configuration_request_transport_and_provider_errors() {
        let unconfigured = registry(None, ClientFactory::plaintext(never_embassy_stack()));
        assert_eq!(
            futures_lite::future::block_on(search_results(
                &unconfigured,
                WebSearchRequest::new("query", 1).unwrap(),
            )),
            Err(WebSearchError::NotConfigured)
        );

        let invalid = registry(
            Some(TavilyConfig {
                api_key: "secret".into(),
                api_base: "http://tavily.test".into(),
            }),
            ClientFactory::plaintext(never_embassy_stack()),
        );
        for request in [
            WebSearchRequest::new("   ", 1).unwrap(),
            WebSearchRequest::new("query", 0).unwrap(),
            WebSearchRequest::new("query", 11).unwrap(),
        ] {
            assert_eq!(
                futures_lite::future::block_on(search_results(&invalid, request)),
                Err(WebSearchError::InvalidRequest)
            );
        }

        let no_tls = registry(
            Some(TavilyConfig {
                api_key: "secret".into(),
                api_base: "https://tavily.test".into(),
            }),
            ClientFactory::plaintext(never_embassy_stack()),
        );
        assert_eq!(
            futures_lite::future::block_on(search_results(
                &no_tls,
                WebSearchRequest::new("query", 1).unwrap(),
            )),
            Err(WebSearchError::Transport)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_rpc_maps_service_malformed_and_oversized_provider_responses() {
        let oversized = vec![b'x'; MAX_RESPONSE_SIZE + 1];
        assert_eq!(
            search_with_provider_response(503, br#"{"error":"unavailable"}"#.to_vec()).await,
            Err(WebSearchError::Service)
        );
        assert_eq!(
            search_with_provider_response(200, br#"{}"#.to_vec()).await,
            Err(WebSearchError::InvalidResponse)
        );
        assert_eq!(
            search_with_provider_response(200, oversized).await,
            Err(WebSearchError::InvalidResponse)
        );
    }
}

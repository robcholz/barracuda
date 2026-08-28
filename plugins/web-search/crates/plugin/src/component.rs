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

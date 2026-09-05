use alloc::{borrow::Cow, boxed::Box, rc::Rc, string::String, vec, vec::Vec};
use core::{cell::RefCell, fmt, future::pending};

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, JsonHandler, JsonPayload, JsonRef, JsonRpcSchema,
    JsonSchema, JsonWriter, RegisterContext, RpcError, RunContext, UnregisterContext, json_schema,
};
use http_client::ClientFactory;
use http_client::reqwless::request::RequestBuilder as _;
use serde::{Deserialize, Deserializer, de::SeqAccess, de::Visitor};

const HEADER_BUFFER_SIZE: usize = 8 * 1024;
const READ_BUFFER_SIZE: usize = 4 * 1024;
const API_REQUEST_BUFFER_SIZE: usize = 2 * 1024;
const MAX_PROVIDER_RESPONSE_BYTES: usize = 64 * 1024;

const MAX_QUERY_BYTES: usize = 255;
const MAX_RESULTS: usize = 10;
const MAX_SEARCH_RESPONSE_BYTES: usize = 512;

const NOT_CONFIGURED_RESPONSE: &str = r#"{"error":"not_configured"}"#;
const BUSY_RESPONSE: &str = r#"{"error":"busy"}"#;
const TRANSPORT_RESPONSE: &str = r#"{"error":"transport"}"#;
const SERVICE_RESPONSE: &str = r#"{"error":"service"}"#;
const INVALID_RESPONSE: &str = r#"{"error":"invalid_response"}"#;

pub(crate) struct TavilyConfig {
    pub(crate) api_key: String,
    pub(crate) search_url: String,
}

/// Starts a Tavily-backed public Web search.
pub struct WebSearch;

impl JsonRpcSchema for WebSearch {
    const ADDRESS: &'static str = "web_search.search";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("search", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("search", response);
    const MAX_REQUEST_BYTES: usize = 320;
    const MAX_RESPONSE_BYTES: usize = MAX_SEARCH_RESPONSE_BYTES;
}

struct SearchWorkspace {
    header_buffer: Box<[u8]>,
    read_buffer: Box<[u8]>,
    request_body: Box<[u8]>,
    response_body: Vec<u8>,
}

impl SearchWorkspace {
    fn new() -> Self {
        Self {
            header_buffer: vec![0; HEADER_BUFFER_SIZE].into_boxed_slice(),
            read_buffer: vec![0; READ_BUFFER_SIZE].into_boxed_slice(),
            request_body: vec![0; API_REQUEST_BUFFER_SIZE].into_boxed_slice(),
            response_body: Vec::with_capacity(READ_BUFFER_SIZE),
        }
    }
}

type SharedWorkspace = Rc<RefCell<Option<SearchWorkspace>>>;

struct WorkspaceLease {
    owner: SharedWorkspace,
    workspace: Option<SearchWorkspace>,
}

impl WorkspaceLease {
    fn acquire(owner: &SharedWorkspace) -> Option<Self> {
        let workspace = owner.borrow_mut().take()?;
        Some(Self {
            owner: Rc::clone(owner),
            workspace: Some(workspace),
        })
    }

    fn get_mut(&mut self) -> Result<&mut SearchWorkspace, RpcError> {
        self.workspace.as_mut().ok_or(RpcError::InvalidFrameState)
    }
}

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        if let Some(workspace) = self.workspace.take() {
            self.owner.replace(Some(workspace));
        }
    }
}

/// Event Router Component serving the awaited Web Search JSON RPC.
pub struct WebSearchComponent {
    config: Rc<RefCell<Option<Rc<TavilyConfig>>>>,
    http_clients: ClientFactory<'static>,
    workspace: SharedWorkspace,
}

impl WebSearchComponent {
    pub(crate) fn new(
        config: Rc<RefCell<Option<Rc<TavilyConfig>>>>,
        http_clients: ClientFactory<'static>,
    ) -> Self {
        Self {
            config,
            http_clients,
            workspace: Rc::new(RefCell::new(Some(SearchWorkspace::new()))),
        }
    }
}

impl<const M: usize> Component<M> for WebSearchComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_json::<WebSearch, _>(
            "*",
            search_handler(
                Rc::clone(&self.config),
                self.http_clients.clone(),
                Rc::clone(&self.workspace),
            ),
        )
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

fn search_handler(
    config: Rc<RefCell<Option<Rc<TavilyConfig>>>>,
    http_clients: ClientFactory<'static>,
    workspace: SharedWorkspace,
) -> impl JsonHandler {
    move |_context, request_json: JsonRef, response: JsonWriter| {
        let config = Rc::clone(&config);
        let http_clients = http_clients.clone();
        let workspace = Rc::clone(&workspace);
        async move {
            let request: SearchRequest<'_> = request_json.deserialize()?;
            validate_request(&request)?;
            let Some(config) = config.borrow().clone() else {
                return response.write(NOT_CONFIGURED_RESPONSE).await;
            };
            let Some(mut workspace) = WorkspaceLease::acquire(&workspace) else {
                return response.write(BUSY_RESPONSE).await;
            };
            let provider_response = fetch(
                &http_clients,
                workspace.get_mut()?,
                &config,
                request.query.as_ref(),
                request.max_results,
            )
            .await;
            match provider_response {
                Ok(provider_response) => {
                    let response_payload = match SearchResponse::new(
                        &provider_response.results.0,
                        request.max_results,
                    ) {
                        Ok(response) => response,
                        Err(error) => return response.write(error.response()).await,
                    };
                    response.write(&response_payload).await
                }
                Err(error) => response.write(error.response()).await,
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchRequest<'a> {
    #[serde(borrow)]
    query: Cow<'a, str>,
    max_results: u8,
}

fn validate_request(request: &SearchRequest<'_>) -> Result<(), RpcError> {
    if request.query.trim().is_empty()
        || request.query.len() > MAX_QUERY_BYTES
        || !(1..=MAX_RESULTS as u8).contains(&request.max_results)
    {
        return Err(RpcError::InvalidJson);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchFailure {
    Transport,
    Service,
    InvalidResponse,
}

impl SearchFailure {
    const fn response(self) -> &'static str {
        match self {
            Self::Transport => TRANSPORT_RESPONSE,
            Self::Service => SERVICE_RESPONSE,
            Self::InvalidResponse => INVALID_RESPONSE,
        }
    }
}

struct SearchResponse<'a> {
    results: &'a [ApiResult<'a>],
    included: usize,
    final_content: Option<&'a str>,
    truncated: bool,
}

impl<'a> SearchResponse<'a> {
    fn new(results: &'a [ApiResult<'a>], max_results: u8) -> Result<Self, SearchFailure> {
        let requested = results
            .get(..core::cmp::min(results.len(), usize::from(max_results)))
            .ok_or(SearchFailure::InvalidResponse)?;
        for result in requested {
            if !result.score.is_finite() || !(0.0..=1.0).contains(&result.score) {
                return Err(SearchFailure::InvalidResponse);
            }
        }

        let mut response = Self {
            results: requested,
            included: 0,
            final_content: None,
            truncated: false,
        };
        for index in 0..requested.len() {
            response.included = index.saturating_add(1);
            if encoded_json_len(&response).is_ok_and(|length| length <= MAX_SEARCH_RESPONSE_BYTES) {
                continue;
            }

            response.truncated = true;
            response.final_content = Some("");
            let empty_length =
                encoded_json_len(&response).map_err(|_error| SearchFailure::InvalidResponse)?;
            if empty_length > MAX_SEARCH_RESPONSE_BYTES {
                response.included = index;
                response.final_content = None;
                return Ok(response);
            }
            let content_budget = MAX_SEARCH_RESPONSE_BYTES.saturating_sub(empty_length);
            let content = requested
                .get(index)
                .map(|result| result.content.as_ref())
                .ok_or(SearchFailure::InvalidResponse)?;
            response.final_content = Some(json_bounded_prefix(content, content_budget));
            return Ok(response);
        }
        Ok(response)
    }
}

impl JsonPayload for SearchResponse<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        let length = encoded_json_len(self)?;
        ensure_bound(length, MAX_SEARCH_RESPONSE_BYTES)?;
        Ok(length)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        self.encoded_len()?;
        write_encoded_json(self, destination)
    }
}

impl EncodedJson for SearchResponse<'_> {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        writer.write_str("{\"results\":[")?;
        for (index, result) in self.results.iter().take(self.included).enumerate() {
            if index > 0 {
                writer.write_char(',')?;
            }
            let content = if index.saturating_add(1) == self.included {
                self.final_content.unwrap_or(result.content.as_ref())
            } else {
                result.content.as_ref()
            };
            encode_result(writer, result, content)?;
        }
        write!(writer, "],\"truncated\":{}}}", self.truncated)
    }
}

fn encode_result(
    writer: &mut impl fmt::Write,
    result: &ApiResult<'_>,
    content: &str,
) -> fmt::Result {
    writer.write_str("{\"title\":")?;
    write_json_string(writer, &result.title)?;
    writer.write_str(",\"url\":")?;
    write_json_string(writer, &result.url)?;
    writer.write_str(",\"content\":")?;
    write_json_string(writer, content)?;
    write!(writer, ",\"score\":{}}}", result.score)
}

struct ApiRequest<'a> {
    api_key: &'a str,
    query: &'a str,
    max_results: u8,
}

impl JsonPayload for ApiRequest<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        encoded_json_len(self)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_encoded_json(self, destination)
    }
}

impl EncodedJson for ApiRequest<'_> {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        writer.write_str("{\"api_key\":")?;
        write_json_string(writer, self.api_key)?;
        writer.write_str(",\"query\":")?;
        write_json_string(writer, self.query)?;
        writer.write_str(",\"search_depth\":\"advanced\",\"max_results\":")?;
        write!(writer, "{}}}", self.max_results)
    }
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
struct ApiResponse<'a> {
    #[serde(borrow)]
    results: BoundedResults<'a>,
}

struct BoundedResults<'a>(Vec<ApiResult<'a>>);

impl<'de: 'a, 'a> Deserialize<'de> for BoundedResults<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedResultsVisitor(core::marker::PhantomData))
    }
}

struct BoundedResultsVisitor<'a>(core::marker::PhantomData<&'a ()>);

impl<'de: 'a, 'a> Visitor<'de> for BoundedResultsVisitor<'a> {
    type Value = BoundedResults<'a>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "at most {MAX_RESULTS} Web Search results")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut results = Vec::with_capacity(MAX_RESULTS);
        while let Some(result) = sequence.next_element::<ApiResult<'a>>()? {
            if results.len() == MAX_RESULTS {
                return Err(serde::de::Error::custom("too many Web Search results"));
            }
            results.push(result);
        }
        Ok(BoundedResults(results))
    }
}

#[derive(Deserialize)]
struct ApiResult<'a> {
    #[serde(borrow)]
    title: Cow<'a, str>,
    #[serde(borrow)]
    url: Cow<'a, str>,
    #[serde(borrow)]
    content: Cow<'a, str>,
    score: f32,
}

async fn fetch<'a>(
    http_clients: &ClientFactory<'static>,
    workspace: &'a mut SearchWorkspace,
    config: &TavilyConfig,
    query: &str,
    max_results: u8,
) -> Result<ApiResponse<'a>, SearchFailure> {
    let body = ApiRequest {
        api_key: &config.api_key,
        query,
        max_results,
    };
    let body_length = body
        .write_json(&mut workspace.request_body)
        .map_err(|_error| SearchFailure::Transport)?;
    let body = workspace
        .request_body
        .get(..body_length)
        .ok_or(SearchFailure::Transport)?;
    let (mut client, tls_configured) = http_clients.create();
    if config.search_url.starts_with("https://") && !tls_configured {
        return Err(SearchFailure::Transport);
    }
    let request = client
        .request(
            http_client::reqwless::request::Method::POST,
            &config.search_url,
        )
        .await
        .map_err(|_error| SearchFailure::Transport)?;
    let mut request = request
        .content_type(http_client::reqwless::headers::ContentType::ApplicationJson)
        .body(body);
    let response = request
        .send(&mut workspace.header_buffer)
        .await
        .map_err(|_error| SearchFailure::Transport)?;
    if !(200..300).contains(&response.status.0) {
        log::warn!("Tavily search returned HTTP {}", response.status.0);
        return Err(SearchFailure::Service);
    }
    if response
        .content_length
        .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES)
    {
        return Err(SearchFailure::InvalidResponse);
    }
    workspace.response_body.clear();
    if let Some(length) = response.content_length {
        let additional = length.saturating_sub(workspace.response_body.capacity());
        workspace
            .response_body
            .try_reserve_exact(additional)
            .map_err(|_error| SearchFailure::InvalidResponse)?;
    }
    let mut reader = response.body().reader();
    loop {
        let read = embedded_io_async::Read::read(&mut reader, &mut workspace.read_buffer)
            .await
            .map_err(|_error| SearchFailure::Transport)?;
        if read == 0 {
            break;
        }
        let length = workspace
            .response_body
            .len()
            .checked_add(read)
            .ok_or(SearchFailure::InvalidResponse)?;
        if length > MAX_PROVIDER_RESPONSE_BYTES {
            return Err(SearchFailure::InvalidResponse);
        }
        let additional = length.saturating_sub(workspace.response_body.capacity());
        workspace
            .response_body
            .try_reserve_exact(additional)
            .map_err(|_error| SearchFailure::InvalidResponse)?;
        workspace.response_body.extend_from_slice(
            workspace
                .read_buffer
                .get(..read)
                .ok_or(SearchFailure::InvalidResponse)?,
        );
    }
    serde_json::from_slice(&workspace.response_body)
        .map_err(|_error| SearchFailure::InvalidResponse)
}

trait EncodedJson {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result;
}

fn encoded_json_len(value: &impl EncodedJson) -> Result<usize, RpcError> {
    let mut writer = LengthWriter::default();
    value
        .encode(&mut writer)
        .map_err(|_error| RpcError::InvalidFrameState)?;
    Ok(writer.written)
}

fn write_encoded_json(value: &impl EncodedJson, destination: &mut [u8]) -> Result<usize, RpcError> {
    let size = encoded_json_len(value)?;
    let capacity = destination.len();
    let mut writer = SliceWriter::new(destination);
    value
        .encode(&mut writer)
        .map_err(|_error| RpcError::FrameTooLarge { size, capacity })?;
    Ok(writer.written)
}

fn write_json_string(writer: &mut impl fmt::Write, value: &str) -> fmt::Result {
    writer.write_char('"')?;
    for character in value.chars() {
        match character {
            '"' => writer.write_str("\\\"")?,
            '\\' => writer.write_str("\\\\")?,
            control @ '\u{0000}'..='\u{001f}' => write!(writer, "\\u{:04x}", u32::from(control))?,
            other => writer.write_char(other)?,
        }
    }
    writer.write_char('"')
}

#[derive(Default)]
struct LengthWriter {
    written: usize,
}

impl fmt::Write for LengthWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.written = self.written.checked_add(value.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}

struct SliceWriter<'a> {
    destination: &'a mut [u8],
    written: usize,
}

impl<'a> SliceWriter<'a> {
    const fn new(destination: &'a mut [u8]) -> Self {
        Self {
            destination,
            written: 0,
        }
    }
}

impl fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.written.checked_add(value.len()).ok_or(fmt::Error)?;
        let destination = self
            .destination
            .get_mut(self.written..end)
            .ok_or(fmt::Error)?;
        destination.copy_from_slice(value.as_bytes());
        self.written = end;
        Ok(())
    }
}

fn ensure_bound(length: usize, capacity: usize) -> Result<(), RpcError> {
    if length > capacity {
        return Err(RpcError::FrameTooLarge {
            size: length,
            capacity,
        });
    }
    Ok(())
}

fn json_bounded_prefix(value: &str, max_encoded_bytes: usize) -> &str {
    let mut encoded = 0_usize;
    let mut end = 0_usize;
    for (index, character) in value.char_indices() {
        let bytes = match character {
            '"' | '\\' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => character.len_utf8(),
        };
        let Some(next) = encoded.checked_add(bytes) else {
            break;
        };
        if next > max_encoded_bytes {
            break;
        }
        encoded = next;
        end = index.saturating_add(character.len_utf8());
    }
    value.get(..end).unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloc::format;
    use serde_json::Value;

    #[test]
    fn rpc_contract_is_one_bounded_unary_response() {
        assert_eq!(WebSearch::ADDRESS, "web_search.search");
        assert_eq!(WebSearch::MAX_REQUEST_BYTES, 320);
        assert_eq!(WebSearch::MAX_RESPONSE_BYTES, 512);
    }

    #[test]
    fn request_validation_enforces_query_and_result_bounds() {
        for request in [
            SearchRequest {
                query: Cow::Borrowed(""),
                max_results: 1,
            },
            SearchRequest {
                query: Cow::Borrowed("   "),
                max_results: 1,
            },
            SearchRequest {
                query: Cow::Owned("x".repeat(MAX_QUERY_BYTES + 1)),
                max_results: 1,
            },
            SearchRequest {
                query: Cow::Borrowed("rust"),
                max_results: 0,
            },
            SearchRequest {
                query: Cow::Borrowed("rust"),
                max_results: 11,
            },
        ] {
            assert_eq!(validate_request(&request), Err(RpcError::InvalidJson));
        }
        assert!(
            validate_request(&SearchRequest {
                query: Cow::Borrowed("embedded rust"),
                max_results: 10,
            })
            .is_ok()
        );
    }

    #[test]
    fn workspace_has_one_cancellation_safe_lease() {
        let workspace = Rc::new(RefCell::new(Some(SearchWorkspace::new())));
        let first = WorkspaceLease::acquire(&workspace).expect("first lease");
        assert!(WorkspaceLease::acquire(&workspace).is_none());
        drop(first);
        assert!(WorkspaceLease::acquire(&workspace).is_some());
    }

    #[test]
    fn successful_response_contains_results_directly() {
        let results = BoundedResults(vec![ApiResult {
            title: Cow::Borrowed("Barracuda"),
            url: Cow::Borrowed("https://example.com/barracuda"),
            content: Cow::Borrowed("Embedded Agent runtime"),
            score: 0.75,
        }]);
        let response = SearchResponse::new(&results.0, 1).expect("valid provider response");
        let encoded = encode_for_test(&response);
        let decoded: Value = serde_json::from_slice(&encoded).expect("valid response JSON");

        assert_eq!(decoded["results"][0]["title"], "Barracuda");
        assert_eq!(
            decoded["results"][0]["url"],
            "https://example.com/barracuda"
        );
        assert_eq!(decoded["results"][0]["content"], "Embedded Agent runtime");
        assert_eq!(decoded["results"][0]["score"], 0.75);
        assert_eq!(decoded["truncated"], false);
    }

    #[test]
    fn total_response_budget_shortens_content_without_cutting_the_url() {
        let url = "https://example.com/a/complete/url";
        let content = "雪".repeat(400);
        let results = BoundedResults(vec![ApiResult {
            title: Cow::Borrowed("A complete title"),
            url: Cow::Borrowed(url),
            content: Cow::Owned(content.clone()),
            score: 0.5,
        }]);
        let response = SearchResponse::new(&results.0, 1).expect("valid provider response");
        let encoded = encode_for_test(&response);
        let decoded: Value = serde_json::from_slice(&encoded).expect("valid response JSON");

        assert!(encoded.len() <= WebSearch::MAX_RESPONSE_BYTES);
        assert_eq!(decoded["results"][0]["url"], url);
        assert!(decoded["results"][0]["content"].as_str().unwrap().len() < content.len());
        assert_eq!(decoded["truncated"], true);
    }

    #[test]
    fn oversized_metadata_is_omitted_instead_of_returning_a_broken_url() {
        let url = format!("https://example.com/{}", "x".repeat(600));
        let results = BoundedResults(vec![ApiResult {
            title: Cow::Borrowed("title"),
            url: Cow::Owned(url),
            content: Cow::Borrowed("content"),
            score: 0.5,
        }]);
        let response = SearchResponse::new(&results.0, 1).expect("valid provider response");
        let encoded = encode_for_test(&response);
        let decoded: Value = serde_json::from_slice(&encoded).expect("valid response JSON");

        assert!(encoded.len() <= WebSearch::MAX_RESPONSE_BYTES);
        assert_eq!(decoded["results"].as_array().unwrap().len(), 0);
        assert_eq!(decoded["truncated"], true);
    }

    #[test]
    fn invalid_scores_reject_the_provider_response() {
        for score in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            let results = BoundedResults(vec![ApiResult {
                title: Cow::Borrowed("title"),
                url: Cow::Borrowed("https://example.com"),
                content: Cow::Borrowed("content"),
                score,
            }]);
            assert!(matches!(
                SearchResponse::new(&results.0, 1),
                Err(SearchFailure::InvalidResponse)
            ));
        }
    }

    #[test]
    fn provider_results_are_borrowed_and_capped_at_ten() {
        let response: ApiResponse<'_> = serde_json::from_str(
            r#"{"results":[{"title":"A","url":"https://a","content":"B","score":0.5}],"request_id":"ignored"}"#,
        )
        .expect("valid Tavily response");
        assert!(matches!(response.results.0[0].title, Cow::Borrowed("A")));

        let item = r#"{"title":"A","url":"https://a","content":"B","score":0.5}"#;
        let too_many = format!(r#"{{"results":[{}]}}"#, [item; 11].join(","));
        assert!(serde_json::from_str::<ApiResponse<'_>>(&too_many).is_err());
    }

    fn encode_for_test(payload: &impl JsonPayload) -> Vec<u8> {
        let length = payload.encoded_len().expect("measure JSON");
        let mut output = vec![0; length];
        let written = payload.write_json(&mut output).expect("encode JSON");
        output.truncate(written);
        output
    }
}

use alloc::{borrow::Cow, boxed::Box, rc::Rc, string::String, vec, vec::Vec};
use core::{cell::RefCell, fmt};

use barracuda_agent_plugin::tools::{ToolError, ToolFuture, ToolHandler, ToolOutput, ToolSpec};
use http_client::ClientFactory;
use http_client::reqwless::request::RequestBuilder as _;
use serde::{Deserialize, Deserializer, Serialize, de::SeqAccess, de::Visitor};

const HEADER_BUFFER_SIZE: usize = 8 * 1024;
const READ_BUFFER_SIZE: usize = 4 * 1024;
const API_REQUEST_BUFFER_SIZE: usize = 2 * 1024;
const MAX_PROVIDER_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_RESULTS: usize = 10;

pub(crate) struct TavilyConfig {
    pub(crate) api_key: String,
    pub(crate) search_url: String,
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

    fn get_mut(&mut self) -> Result<&mut SearchWorkspace, SearchFailure> {
        self.workspace
            .as_mut()
            .ok_or(SearchFailure::InvalidResponse)
    }
}

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        if let Some(workspace) = self.workspace.take() {
            self.owner.replace(Some(workspace));
        }
    }
}

/// Awaited Agent Tool backed by Tavily Web Search.
pub(crate) struct WebSearchTool {
    config: Rc<RefCell<Option<Rc<TavilyConfig>>>>,
    http_clients: ClientFactory<'static>,
    workspace: SharedWorkspace,
}

impl WebSearchTool {
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

impl ToolSpec for WebSearchTool {
    barracuda_agent_plugin::tools::tool_metadata!("web_search");
}

impl ToolHandler for WebSearchTool {
    type Args = SearchRequest;

    fn invoke<'a>(&'a self, request: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let Some(config) = self.config.borrow().clone() else {
                return Ok(SearchFailure::NotConfigured.into_output());
            };
            let Some(mut workspace) = WorkspaceLease::acquire(&self.workspace) else {
                return Ok(SearchFailure::Busy.into_output());
            };
            let workspace = match workspace.get_mut() {
                Ok(workspace) => workspace,
                Err(error) => return Ok(error.into_output()),
            };
            let response = fetch(
                &self.http_clients,
                workspace,
                &config,
                &request.query,
                request.max_results,
            )
            .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => return Ok(error.into_output()),
            };
            let results = match response.results.requested(request.max_results) {
                Ok(results) => results,
                Err(error) => return Ok(error.into_output()),
            };
            let content = serde_json::to_string(&SearchResponse {
                results,
                truncated: false,
            })
            .map_err(|_error| {
                ToolError::InvokeRejected(String::from("failed to encode Web Search response"))
            })?;
            Ok(ToolOutput { content, ok: true })
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchRequest {
    query: String,
    max_results: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchFailure {
    NotConfigured,
    Busy,
    Transport,
    Service,
    InvalidResponse,
}

impl SearchFailure {
    const fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Busy => "busy",
            Self::Transport => "transport",
            Self::Service => "service",
            Self::InvalidResponse => "invalid_response",
        }
    }

    fn into_output(self) -> ToolOutput {
        ToolOutput {
            content: alloc::format!(r#"{{"error":"{}"}}"#, self.code()),
            ok: false,
        }
    }
}

#[derive(Serialize)]
struct SearchResponse<'a, Item> {
    results: &'a [Item],
    truncated: bool,
}

struct ApiRequest<'a> {
    api_key: &'a str,
    query: &'a str,
    max_results: u8,
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

impl BoundedResults<'_> {
    fn requested(&self, max_results: u8) -> Result<&[ApiResult<'_>], SearchFailure> {
        let results = self
            .0
            .get(..core::cmp::min(self.0.len(), usize::from(max_results)))
            .ok_or(SearchFailure::InvalidResponse)?;
        if results
            .iter()
            .any(|result| !result.score.is_finite() || !(0.0..=1.0).contains(&result.score))
        {
            return Err(SearchFailure::InvalidResponse);
        }
        Ok(results)
    }
}

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

#[derive(Deserialize, Serialize)]
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
    let body_length = write_encoded_json(&body, &mut workspace.request_body)
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

fn write_encoded_json(
    value: &impl EncodedJson,
    destination: &mut [u8],
) -> Result<usize, fmt::Error> {
    let mut writer = SliceWriter::new(destination);
    value.encode(&mut writer)?;
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn provider_results_are_borrowed_and_capped_at_ten() {
        let response: ApiResponse<'_> = serde_json::from_str(
            r#"{"results":[{"title":"A","url":"https://a","content":"B","score":0.5}],"request_id":"ignored"}"#,
        )
        .expect("valid Tavily response");
        assert!(matches!(response.results.0[0].title, Cow::Borrowed("A")));

        let item = r#"{"title":"A","url":"https://a","content":"B","score":0.5}"#;
        let too_many = alloc::format!(r#"{{"results":[{}]}}"#, [item; 11].join(","));
        assert!(serde_json::from_str::<ApiResponse<'_>>(&too_many).is_err());
    }
}

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use barracuda_bulk_memory::{BulkBox, BulkMemoryError, BulkVec};
use core::cell::RefCell;
use embassy_time::{Duration, with_timeout};

use http_client::{
    ClientFactory,
    embedded_nal_async::{Dns, TcpConnect},
    reqwless::request::{Method, RequestBuilder as _},
};
use serde::{Deserialize, Serialize};

const HEADER_BUFFER_SIZE: usize = 16 * 1024;
const READ_BUFFER_SIZE: usize = 4 * 1024;
const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024;
const MAX_REQUEST_URL_BYTES: usize = 2 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 32 * 1024;
const MAX_REQUEST_HEADERS: usize = 32;
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const REQUEST_TIMEOUT_MILLIS: u64 = 30_000;
const WORKSPACE_CAPACITY: usize = 2;

/// HTTP method supported by [`Http::request`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
    /// HEAD.
    Head,
}

impl From<HttpMethod> for Method {
    fn from(method: HttpMethod) -> Self {
        match method {
            HttpMethod::Get => Self::GET,
            HttpMethod::Post => Self::POST,
            HttpMethod::Put => Self::PUT,
            HttpMethod::Patch => Self::PATCH,
            HttpMethod::Delete => Self::DELETE,
            HttpMethod::Head => Self::HEAD,
        }
    }
}

/// One ordered outbound HTTP header.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpHeader {
    /// Header name.
    pub name: String,
    /// Header value.
    pub value: String,
}

/// Typed outbound HTTP request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRequest {
    /// Request method.
    pub method: HttpMethod,
    /// Absolute HTTP or HTTPS URL.
    pub url: String,
    /// Ordered request headers.
    #[serde(default)]
    pub headers: Vec<HttpHeader>,
    /// UTF-8 request body.
    #[serde(default)]
    pub body: String,
}

impl HttpRequest {
    fn validate(&self, tls: bool) -> Result<(), HttpError> {
        let header_bytes = self.headers.iter().try_fold(0_usize, |total, header| {
            total
                .checked_add(header.name.len())?
                .checked_add(header.value.len())
        });
        if self.url.len() > MAX_REQUEST_URL_BYTES
            || self.body.len() > MAX_REQUEST_BODY_BYTES
            || self.headers.len() > MAX_REQUEST_HEADERS
            || header_bytes.is_none_or(|bytes| bytes > MAX_REQUEST_HEADER_BYTES)
        {
            return Err(HttpError::RequestTooLarge);
        }
        if !self.url.starts_with("http://") && !self.url.starts_with("https://") {
            return Err(HttpError::InvalidUrl);
        }
        if self.url.starts_with("https://") && !tls {
            return Err(HttpError::TlsNotConfigured);
        }
        for header in &self.headers {
            if header.name.is_empty()
                || header.name.bytes().any(|byte| byte <= b' ' || byte == b':')
                || header.value.contains('\r')
                || header.value.contains('\n')
            {
                return Err(HttpError::InvalidHeader);
            }
        }
        Ok(())
    }
}

/// Successful buffered HTTP response.
#[derive(Debug, Eq, PartialEq, Serialize)]
pub struct HttpResponse {
    /// Upstream HTTP status.
    pub status: u16,
    /// Complete UTF-8 response body.
    pub body: String,
}

/// Stable outbound HTTP failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum HttpError {
    /// URL is not absolute HTTP or HTTPS.
    #[error("invalid URL")]
    InvalidUrl,
    /// HTTPS was requested without configured TLS.
    #[error("TLS is not configured")]
    TlsNotConfigured,
    /// A request header is invalid.
    #[error("invalid header")]
    InvalidHeader,
    /// URL, headers, or body exceed the bounded request capacity.
    #[error("HTTP request is too large")]
    RequestTooLarge,
    /// Both reusable HTTP workspaces are occupied.
    #[error("HTTP request capacity is busy")]
    Busy,
    /// The complete request exceeded its deadline.
    #[error("HTTP request timed out")]
    Timeout,
    /// DNS, TCP, TLS, HTTP, or body reading failed.
    #[error("HTTP transport failed")]
    Transport,
    /// Upstream body is not UTF-8.
    #[error("response body is not UTF-8")]
    InvalidResponseText,
    /// Upstream body exceeds the defensive bound.
    #[error("response body is too large")]
    ResponseTooLarge,
    /// Bulk scratch memory could not be allocated.
    #[error("HTTP scratch memory allocation failed")]
    Allocation,
}

impl HttpError {
    /// Stable machine-readable error code used by external adapters.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidUrl => "invalid_url",
            Self::TlsNotConfigured => "tls_not_configured",
            Self::InvalidHeader => "invalid_header",
            Self::RequestTooLarge => "request_too_large",
            Self::Busy => "busy",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::InvalidResponseText => "invalid_response_text",
            Self::ResponseTooLarge => "response_too_large",
            Self::Allocation => "allocation",
        }
    }
}

struct HttpWorkspace {
    header_buffer: BulkBox<[u8]>,
    read_buffer: BulkBox<[u8]>,
    response_body: BulkVec<u8>,
}

impl HttpWorkspace {
    fn try_new() -> Result<Self, BulkMemoryError> {
        Ok(Self {
            header_buffer: BulkBox::try_zeroed_slice(HEADER_BUFFER_SIZE)?,
            read_buffer: BulkBox::try_zeroed_slice(READ_BUFFER_SIZE)?,
            response_body: BulkVec::try_with_capacity(READ_BUFFER_SIZE)?,
        })
    }
}

struct WorkspacePool {
    slots: RefCell<[Option<Box<HttpWorkspace>>; WORKSPACE_CAPACITY]>,
}

impl WorkspacePool {
    fn try_new() -> Result<Self, BulkMemoryError> {
        let mut slots = core::array::from_fn(|_| None);
        for slot in &mut slots {
            *slot = Some(Box::new(HttpWorkspace::try_new()?));
        }
        Ok(Self {
            slots: RefCell::new(slots),
        })
    }

    fn acquire(self: &Rc<Self>) -> Result<WorkspaceLease, HttpError> {
        let workspace = self
            .slots
            .borrow_mut()
            .iter_mut()
            .find_map(Option::take)
            .ok_or(HttpError::Busy)?;
        Ok(WorkspaceLease {
            pool: Rc::clone(self),
            workspace: Some(workspace),
        })
    }
}

struct WorkspaceLease {
    pool: Rc<WorkspacePool>,
    workspace: Option<Box<HttpWorkspace>>,
}

impl WorkspaceLease {
    fn get_mut(&mut self) -> Result<&mut HttpWorkspace, HttpError> {
        self.workspace.as_deref_mut().ok_or(HttpError::Transport)
    }
}

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        let Some(workspace) = self.workspace.take() else {
            return;
        };
        if let Some(slot) = self
            .pool
            .slots
            .borrow_mut()
            .iter_mut()
            .find(|slot| slot.is_none())
        {
            *slot = Some(workspace);
        }
    }
}

/// Shared typed capability for buffered outbound HTTP requests.
pub struct Http<T: 'static = http_client::Tcp, D: 'static = http_client::Resolver> {
    clients: ClientFactory<'static, T, D>,
    workspaces: Rc<WorkspacePool>,
}

impl<T, D> Http<T, D> {
    /// Creates the capability from System's shared HTTP client factory.
    ///
    /// # Errors
    ///
    /// Returns an error when the reusable bulk-memory workspaces cannot be
    /// allocated.
    pub fn try_new(clients: ClientFactory<'static, T, D>) -> Result<Self, HttpError> {
        Ok(Self {
            clients,
            workspaces: Rc::new(WorkspacePool::try_new().map_err(|_error| HttpError::Allocation)?),
        })
    }
}

impl<T: TcpConnect + 'static, D: Dns + 'static> Http<T, D> {
    /// Executes one buffered outbound request.
    pub async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        with_timeout(
            Duration::from_millis(REQUEST_TIMEOUT_MILLIS),
            self.request_inner(request),
        )
        .await
        .map_err(|_timeout| HttpError::Timeout)?
    }

    async fn request_inner(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut workspace = self.workspaces.acquire()?;
        let workspace = workspace.get_mut()?;
        let (mut client, tls) = self.clients.create();
        request.validate(tls)?;
        let headers = request
            .headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
            .collect::<Vec<_>>();
        let outbound = client
            .request(request.method.into(), &request.url)
            .await
            .map_err(|_error| HttpError::Transport)?;
        let mut outbound = outbound.headers(&headers).body(request.body.as_bytes());
        let response = outbound
            .send(&mut workspace.header_buffer)
            .await
            .map_err(|_error| HttpError::Transport)?;
        let status = response.status.0;
        if !(100..=599).contains(&status) {
            return Err(HttpError::Transport);
        }
        workspace.response_body.clear();
        let mut reader = response.body().reader();
        loop {
            let read = embedded_io_async::Read::read(&mut reader, &mut workspace.read_buffer)
                .await
                .map_err(|_error| HttpError::Transport)?;
            if read == 0 {
                break;
            }
            let next_len = workspace
                .response_body
                .len()
                .checked_add(read)
                .ok_or(HttpError::ResponseTooLarge)?;
            if next_len > MAX_RESPONSE_BODY_BYTES {
                return Err(HttpError::ResponseTooLarge);
            }
            workspace
                .response_body
                .try_extend_from_slice(
                    workspace
                        .read_buffer
                        .get(..read)
                        .ok_or(HttpError::Transport)?,
                )
                .map_err(|_error| HttpError::Allocation)?;
        }
        let body = core::str::from_utf8(&workspace.response_body)
            .map_err(|_error| HttpError::InvalidResponseText)?;
        Ok(HttpResponse {
            status,
            body: String::from(body),
        })
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec::Vec};

    use super::{
        HttpError, HttpHeader, HttpMethod, HttpRequest, MAX_REQUEST_BODY_BYTES,
        MAX_REQUEST_HEADER_BYTES, MAX_REQUEST_HEADERS, MAX_REQUEST_URL_BYTES,
    };

    fn request() -> HttpRequest {
        HttpRequest {
            method: HttpMethod::Get,
            url: String::from("https://example.com"),
            headers: Vec::new(),
            body: String::new(),
        }
    }

    #[test]
    fn validates_bounded_requests() {
        let valid = request();
        assert_eq!(valid.validate(true), Ok(()));

        let mut oversized_url = request();
        oversized_url.url = "u".repeat(MAX_REQUEST_URL_BYTES + 1);
        assert_eq!(
            oversized_url.validate(true),
            Err(HttpError::RequestTooLarge)
        );

        let mut oversized_body = request();
        oversized_body.body = "b".repeat(MAX_REQUEST_BODY_BYTES + 1);
        assert_eq!(
            oversized_body.validate(true),
            Err(HttpError::RequestTooLarge)
        );

        let mut too_many_headers = request();
        too_many_headers.headers = (0..=MAX_REQUEST_HEADERS)
            .map(|_| HttpHeader {
                name: String::from("x"),
                value: String::new(),
            })
            .collect();
        assert_eq!(
            too_many_headers.validate(true),
            Err(HttpError::RequestTooLarge)
        );

        let mut oversized_headers = request();
        oversized_headers.headers.push(HttpHeader {
            name: String::from("x"),
            value: "v".repeat(MAX_REQUEST_HEADER_BYTES),
        });
        assert_eq!(
            oversized_headers.validate(true),
            Err(HttpError::RequestTooLarge)
        );
    }

    #[test]
    fn distinguishes_invalid_requests_from_missing_tls() {
        let mut invalid = request();
        invalid.url = String::from("file:///data/value");
        assert_eq!(invalid.validate(true), Err(HttpError::InvalidUrl));

        let missing_tls = request();
        assert_eq!(
            missing_tls.validate(false),
            Err(HttpError::TlsNotConfigured)
        );

        let mut invalid_header = request();
        invalid_header.headers.push(HttpHeader {
            name: String::from("bad:name"),
            value: String::new(),
        });
        assert_eq!(invalid_header.validate(true), Err(HttpError::InvalidHeader));
    }
}

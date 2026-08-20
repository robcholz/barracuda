use alloc::{rc::Rc, string::String, vec::Vec};

use barracuda_agent_runtime::{AgentRuntime, ApiPurpose, BackendKind, ModelApiConfig};
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::wire::{read_payload, write_payload, AgentWireError};

const FRAME_PAYLOAD_SIZE: usize = 126;

/// Logical request corresponding to `AgentRuntime::link_api`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkApiRequest {
    api: ModelApiConfig,
    purpose: ApiPurpose,
    default: bool,
}

impl LinkApiRequest {
    /// Creates an API-link request from Agent domain types.
    #[must_use]
    pub fn new(api: ModelApiConfig, purpose: ApiPurpose, default: bool) -> Self {
        Self {
            api,
            purpose,
            default,
        }
    }

    pub(crate) fn into_parts(self) -> (ModelApiConfig, ApiPurpose, bool) {
        (self.api, self.purpose, self.default)
    }
}

/// One frame of a variable-length [`LinkApiRequest`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct LinkApiRequestFrame {
    length: u16,
    payload: [u8; FRAME_PAYLOAD_SIZE],
}

impl LinkApiRequestFrame {
    fn new(bytes: &[u8]) -> Result<Self, AgentWireError> {
        let mut payload = [0; FRAME_PAYLOAD_SIZE];
        let length = write_payload(&mut payload, bytes)?;
        Ok(Self { length, payload })
    }

    fn bytes(&self) -> Result<&[u8], AgentWireError> {
        read_payload(&self.payload, self.length)
    }
}

/// Failure returned by `agent.link_api`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum LinkApiError {
    /// The frame stream or encoded request is invalid.
    InvalidRequest,
    /// The supplied model API configuration is invalid.
    InvalidConfiguration,
}

/// RPC corresponding to `AgentRuntime::link_api`.
pub struct LinkApi;

impl RpcMethod for LinkApi {
    const ADDRESS: &'static str = "agent.link_api";
    type Request = LinkApiRequestFrame;
    type Response = ();
    type Error = LinkApiError;
    type Input = Streaming;
    type Output = Unary;
}

/// Builds the reusable handler for [`LinkApi`].
pub fn link_api_handler<Filesystem, Http>(
    runtime: Rc<AgentRuntime<Filesystem, Http>>,
) -> impl RpcHandler<LinkApi>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    move |_context, mut frames: RpcStream<RpcFrame<LinkApiRequestFrame>>| {
        let runtime = Rc::clone(&runtime);
        async move {
            let mut chunks = Vec::new();
            while let Some(frame) = frames.next().await {
                chunks.push(*frame?.view()?);
            }
            let request = match link_api_request_from_frames(chunks) {
                Ok(request) => request,
                Err(_error) => return Ok(Err(LinkApiError::InvalidRequest)),
            };
            let (api, purpose, default) = request.into_parts();
            match runtime.link_api(api, purpose, default) {
                Ok(()) => Ok(Ok(())),
                Err(_error) => Ok(Err(LinkApiError::InvalidConfiguration)),
            }
        }
    }
}

/// Encodes one logical API-link request into transport frames.
pub fn frames_from_link_api_request(
    request: &LinkApiRequest,
) -> Result<Vec<LinkApiRequestFrame>, AgentWireError> {
    let wire = LinkApiRequestDto::try_from(request)?;
    serde_json::to_vec(&wire)?
        .chunks(FRAME_PAYLOAD_SIZE)
        .map(LinkApiRequestFrame::new)
        .collect()
}

/// Decodes one logical API-link request from transport frames.
pub fn link_api_request_from_frames(
    frames: impl IntoIterator<Item = LinkApiRequestFrame>,
) -> Result<LinkApiRequest, AgentWireError> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend_from_slice(frame.bytes()?);
    }
    LinkApiRequest::try_from(serde_json::from_slice::<LinkApiRequestDto>(&bytes)?)
}

#[derive(Deserialize, Serialize)]
struct LinkApiRequestDto {
    backend: BackendKind,
    api_key: String,
    model: String,
    base_url: String,
    timeout_ms: u32,
    max_tokens: u32,
    image_max_bytes: u64,
    purpose: ApiPurposeDto,
    default: bool,
}

impl TryFrom<&LinkApiRequest> for LinkApiRequestDto {
    type Error = AgentWireError;

    fn try_from(request: &LinkApiRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            backend: request.api.backend,
            api_key: request.api.api_key.clone(),
            model: request.api.model.clone(),
            base_url: request.api.base_url.clone(),
            timeout_ms: request.api.timeout_ms,
            max_tokens: request.api.max_tokens,
            image_max_bytes: u64::try_from(request.api.image_max_bytes)
                .map_err(|_| AgentWireError::InvalidFrame)?,
            purpose: request.purpose.into(),
            default: request.default,
        })
    }
}

impl TryFrom<LinkApiRequestDto> for LinkApiRequest {
    type Error = AgentWireError;

    fn try_from(request: LinkApiRequestDto) -> Result<Self, Self::Error> {
        let mut api = ModelApiConfig::new(
            request.backend,
            request.api_key,
            request.model,
            request.base_url,
        );
        api.timeout_ms = request.timeout_ms;
        api.max_tokens = request.max_tokens;
        api.image_max_bytes =
            usize::try_from(request.image_max_bytes).map_err(|_| AgentWireError::InvalidFrame)?;
        Ok(Self::new(api, request.purpose.into(), request.default))
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ApiPurposeDto {
    RootAgent,
    SubAgent,
    Memory,
    Compaction,
}

impl From<ApiPurpose> for ApiPurposeDto {
    fn from(value: ApiPurpose) -> Self {
        match value {
            ApiPurpose::RootAgent => Self::RootAgent,
            ApiPurpose::SubAgent => Self::SubAgent,
            ApiPurpose::Memory => Self::Memory,
            ApiPurpose::Compaction => Self::Compaction,
        }
    }
}

impl From<ApiPurposeDto> for ApiPurpose {
    fn from(value: ApiPurposeDto) -> Self {
        match value {
            ApiPurposeDto::RootAgent => Self::RootAgent,
            ApiPurposeDto::SubAgent => Self::SubAgent,
            ApiPurposeDto::Memory => Self::Memory,
            ApiPurposeDto::Compaction => Self::Compaction,
        }
    }
}

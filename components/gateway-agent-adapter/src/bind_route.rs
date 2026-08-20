use alloc::vec::Vec;

use barracuda_agent_runtime::SessionId;
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary};
use barracuda_message_gateway_component::route::GatewayRoute;
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::component::AdapterRegistry;

const FRAME_PAYLOAD_SIZE: usize = 126;

/// Logical configuration request binding one Gateway route to an Agent session.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct BindRouteRequest {
    route: GatewayRoute,
    session: SessionId,
}

impl BindRouteRequest {
    /// Creates a route binding.
    #[must_use]
    pub fn new(route: GatewayRoute, session: SessionId) -> Self {
        Self { route, session }
    }

    fn into_parts(self) -> (GatewayRoute, SessionId) {
        (self.route, self.session)
    }
}

/// One frame of a variable-length [`BindRouteRequest`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct BindRouteRequestFrame {
    length: u16,
    payload: [u8; FRAME_PAYLOAD_SIZE],
}

impl BindRouteRequestFrame {
    fn new(bytes: &[u8]) -> Result<Self, BindRouteWireError> {
        let length = u16::try_from(bytes.len()).map_err(|_| BindRouteWireError::InvalidFrame)?;
        let mut payload = [0; FRAME_PAYLOAD_SIZE];
        payload
            .get_mut(..bytes.len())
            .ok_or(BindRouteWireError::InvalidFrame)?
            .copy_from_slice(bytes);
        Ok(Self { length, payload })
    }

    fn bytes(&self) -> Result<&[u8], BindRouteWireError> {
        self.payload
            .get(..usize::from(self.length))
            .ok_or(BindRouteWireError::InvalidFrame)
    }
}

/// Failure returned by `adapter.bind_route`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum BindRouteError {
    /// The request frames did not contain one valid binding.
    InvalidRequest,
}

/// Configures route/session correlation used by the conversion RPCs.
pub struct BindRoute;

impl RpcMethod for BindRoute {
    const ADDRESS: &'static str = "adapter.bind_route";
    type Request = BindRouteRequestFrame;
    type Response = ();
    type Error = BindRouteError;
    type Input = Streaming;
    type Output = Unary;
}

/// Failure encoding or decoding a bind-route request.
#[derive(Debug, thiserror::Error)]
pub enum BindRouteWireError {
    /// A frame carried an invalid length.
    #[error("invalid bind-route frame")]
    InvalidFrame,
    /// The logical JSON request was invalid.
    #[error("invalid bind-route JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Encodes a logical route binding into frames.
pub fn frames_from_bind_route_request(
    request: &BindRouteRequest,
) -> Result<Vec<BindRouteRequestFrame>, BindRouteWireError> {
    serde_json::to_vec(request)?
        .chunks(FRAME_PAYLOAD_SIZE)
        .map(BindRouteRequestFrame::new)
        .collect()
}

fn bind_route_request_from_frames(
    frames: impl IntoIterator<Item = BindRouteRequestFrame>,
) -> Result<BindRouteRequest, BindRouteWireError> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend_from_slice(frame.bytes()?);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// Builds the reusable handler for [`BindRoute`].
pub fn bind_route_handler(registry: AdapterRegistry) -> impl RpcHandler<BindRoute> {
    move |_context, mut frames: RpcStream<RpcFrame<BindRouteRequestFrame>>| {
        let registry = registry.clone();
        async move {
            let mut chunks = Vec::new();
            while let Some(frame) = frames.next().await {
                chunks.push(*frame?.view()?);
            }
            let request = match bind_route_request_from_frames(chunks) {
                Ok(request) => request,
                Err(_error) => return Ok(Err(BindRouteError::InvalidRequest)),
            };
            let (route, session) = request.into_parts();
            registry.0.borrow_mut().bind(route, session);
            Ok(Ok(()))
        }
    }
}

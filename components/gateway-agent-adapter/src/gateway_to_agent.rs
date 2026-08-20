use alloc::vec::Vec;

use barracuda_agent_component::session::append::{
    frames_from_append_request, Append, AppendRequest, AppendRequestFrame,
};
use barracuda_agent_runtime::Message;
use barracuda_event_router::{Event, RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming};
use barracuda_message_gateway_component::gateway_message_received::{
    gateway_event_from_frames, GatewayEventFrame, GatewayMessageReceived,
};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::component::AdapterRegistry;

/// Business failure returned by `adapter.gateway_to_agent`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum GatewayToAgentError {
    /// The Gateway event was malformed.
    InvalidMessage,
    /// No Agent session is bound to the Gateway route.
    RouteNotBound,
}

/// Converts a Gateway event contract to `session.append` request frames.
pub struct GatewayToAgent;

impl RpcMethod for GatewayToAgent {
    const ADDRESS: &'static str = "adapter.gateway_to_agent";
    type Request = <GatewayMessageReceived as Event>::Message;
    type Response = <Append as RpcMethod>::Request;
    type Error = GatewayToAgentError;
    type Input = Streaming;
    type Output = Streaming;
}

/// Builds the reusable handler for [`GatewayToAgent`].
pub fn gateway_to_agent_handler(registry: AdapterRegistry) -> impl RpcHandler<GatewayToAgent> {
    move |_context, frames: RpcStream<RpcFrame<GatewayEventFrame>>| {
        let registry = registry.clone();
        async move {
            let inbound = match gateway_event_from_frames(collect(frames).await?) {
                Ok(message) => message,
                Err(_error) => return Ok(error_stream(GatewayToAgentError::InvalidMessage)),
            };
            let Some(session) = registry.0.borrow().session_for_route(&inbound.route) else {
                return Ok(error_stream(GatewayToAgentError::RouteNotBound));
            };
            if let Some(binding) = registry.0.borrow_mut().by_session.get_mut(&session) {
                binding.latest_reply_to = Some(inbound.message_id);
            }
            match frames_from_append_request(&AppendRequest::new(
                session,
                Message::text(inbound.text),
            )) {
                Ok(frames) => Ok(success_stream(frames)),
                Err(_error) => Ok(error_stream(GatewayToAgentError::InvalidMessage)),
            }
        }
    }
}

async fn collect(
    mut frames: RpcStream<RpcFrame<GatewayEventFrame>>,
) -> Result<Vec<GatewayEventFrame>, barracuda_event_router::RpcError> {
    let mut collected = Vec::new();
    while let Some(frame) = frames.next().await {
        collected.push(*frame?.view()?);
    }
    Ok(collected)
}

fn success_stream(
    frames: Vec<AppendRequestFrame>,
) -> RpcStream<Result<AppendRequestFrame, GatewayToAgentError>> {
    RpcStream::new(futures_lite::stream::iter(
        frames.into_iter().map(|frame| Ok(Ok(frame))),
    ))
}

fn error_stream(
    error: GatewayToAgentError,
) -> RpcStream<Result<AppendRequestFrame, GatewayToAgentError>> {
    RpcStream::new(futures_lite::stream::iter([Ok(Err(error))]))
}

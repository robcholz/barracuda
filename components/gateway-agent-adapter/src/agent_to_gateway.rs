use alloc::{collections::VecDeque, string::String, vec::Vec};

use barracuda_agent_component::open_session::{
    open_session_response_from_frames, OpenSession, OpenSessionResponse, OpenSessionResponseFrame,
    SessionEventDto,
};
use barracuda_agent_runtime::SessionId;
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming};
use barracuda_message_gateway_component::gateway_send::{
    frames_from_gateway_send, GatewayOutboundMessage, GatewaySend, GatewaySendRequestFrame,
};
use futures_lite::stream;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::component::AdapterRegistry;

/// Business failure returned by `adapter.agent_to_gateway`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum AgentToGatewayError {
    /// The Agent event or generated Gateway request was malformed.
    InvalidMessage,
    /// No Gateway route is bound to the event's Agent session.
    SessionNotBound,
}

/// Converts the `agent.open_session` response contract to `gateway.send` requests.
pub struct AgentToGateway;

impl RpcMethod for AgentToGateway {
    const ADDRESS: &'static str = "adapter.agent_to_gateway";
    type Request = <OpenSession as RpcMethod>::Response;
    type Response = <GatewaySend as RpcMethod>::Request;
    type Error = AgentToGatewayError;
    type Input = Streaming;
    type Output = Streaming;
}

/// Builds the reusable handler for [`AgentToGateway`].
pub fn agent_to_gateway_handler(registry: AdapterRegistry) -> impl RpcHandler<AgentToGateway> {
    move |_context, input: RpcStream<RpcFrame<OpenSessionResponseFrame>>| {
        let stream_state = AgentToGatewayStream {
            input,
            registry: registry.clone(),
            event_frames: Vec::new(),
            pending: VecDeque::new(),
        };
        async move {
            Ok(RpcStream::new(stream::unfold(
                stream_state,
                |mut stream_state| async move {
                    loop {
                        if let Some(frame) = stream_state.pending.pop_front() {
                            return Some((Ok(Ok(frame)), stream_state));
                        }
                        let frame = match stream_state.input.next().await {
                            Some(Ok(frame)) => match frame.view() {
                                Ok(frame) => *frame,
                                Err(error) => return Some((Err(error), stream_state)),
                            },
                            Some(Err(error)) => return Some((Err(error), stream_state)),
                            None => return None,
                        };
                        let end = frame.is_end();
                        stream_state.event_frames.push(frame);
                        if !end {
                            continue;
                        }
                        let response = match open_session_response_from_frames(
                            stream_state.event_frames.drain(..),
                        ) {
                            Ok(response) => response,
                            Err(_error) => {
                                return Some((
                                    Ok(Err(AgentToGatewayError::InvalidMessage)),
                                    stream_state,
                                ))
                            }
                        };
                        match convert_response(&stream_state.registry, response) {
                            Ok(Some(frames)) => stream_state.pending = frames.into(),
                            Ok(None) => {}
                            Err(error) => return Some((Ok(Err(error)), stream_state)),
                        }
                    }
                },
            )))
        }
    }
}

struct AgentToGatewayStream {
    input: RpcStream<RpcFrame<OpenSessionResponseFrame>>,
    registry: AdapterRegistry,
    event_frames: Vec<OpenSessionResponseFrame>,
    pending: VecDeque<GatewaySendRequestFrame>,
}

fn convert_response(
    registry: &AdapterRegistry,
    response: OpenSessionResponse,
) -> Result<Option<Vec<GatewaySendRequestFrame>>, AgentToGatewayError> {
    let OpenSessionResponse::Event { session, event } = response else {
        return Ok(None);
    };
    match event {
        SessionEventDto::OutputDelta { text } | SessionEventDto::EffectOutputDelta { text } => {
            append_output(registry, session, text);
            Ok(None)
        }
        SessionEventDto::TurnEnded { .. } => finish_output(registry, session),
        SessionEventDto::Closed { .. } => {
            registry.0.borrow_mut().output.remove(&session);
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn append_output(registry: &AdapterRegistry, session: SessionId, text: String) {
    registry
        .0
        .borrow_mut()
        .output
        .entry(session)
        .or_default()
        .push_str(&text);
}

fn finish_output(
    registry: &AdapterRegistry,
    session: SessionId,
) -> Result<Option<Vec<GatewaySendRequestFrame>>, AgentToGatewayError> {
    let mut state = registry.0.borrow_mut();
    let text = state.output.remove(&session).unwrap_or_default();
    if text.is_empty() {
        return Ok(None);
    }
    let binding = state
        .by_session
        .get_mut(&session)
        .ok_or(AgentToGatewayError::SessionNotBound)?;
    let message = GatewayOutboundMessage {
        route: binding.route.clone(),
        text,
        reply_to: binding.latest_reply_to.take(),
    };
    frames_from_gateway_send(&message)
        .map(Some)
        .map_err(|_error| AgentToGatewayError::InvalidMessage)
}

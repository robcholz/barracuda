use alloc::vec::Vec;

use barracuda_agent_runtime::{InputRequestId, Message, SessionId};
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::wire::{read_payload, write_payload, AgentWireError};

use super::{SessionRegistry, SessionRpcError};

const FRAME_PAYLOAD_SIZE: usize = 126;

/// Logical request corresponding to `SessionControl::respond`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RespondRequest {
    session: SessionId,
    request: InputRequestId,
    message: Message,
}

impl RespondRequest {
    /// Creates a response to one outstanding input request.
    #[must_use]
    pub fn new(session: SessionId, request: InputRequestId, message: Message) -> Self {
        Self {
            session,
            request,
            message,
        }
    }

    pub(crate) fn into_parts(self) -> (SessionId, InputRequestId, Message) {
        (self.session, self.request, self.message)
    }
}

/// One frame of a variable-length [`RespondRequest`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct RespondRequestFrame {
    length: u16,
    payload: [u8; FRAME_PAYLOAD_SIZE],
}

impl RespondRequestFrame {
    fn new(bytes: &[u8]) -> Result<Self, AgentWireError> {
        let mut payload = [0; FRAME_PAYLOAD_SIZE];
        let length = write_payload(&mut payload, bytes)?;
        Ok(Self { length, payload })
    }

    fn bytes(&self) -> Result<&[u8], AgentWireError> {
        read_payload(&self.payload, self.length)
    }
}

/// RPC corresponding to `SessionControl::respond`.
pub struct Respond;

impl RpcMethod for Respond {
    const ADDRESS: &'static str = "session.respond";
    type Request = RespondRequestFrame;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Streaming;
    type Output = Unary;
}

/// Builds the reusable handler for [`Respond`].
pub fn respond_handler(registry: SessionRegistry) -> impl RpcHandler<Respond> {
    move |_context, mut frames: RpcStream<RpcFrame<RespondRequestFrame>>| {
        let registry = registry.clone();
        async move {
            let mut chunks = Vec::new();
            while let Some(frame) = frames.next().await {
                chunks.push(*frame?.view()?);
            }
            let request = match respond_request_from_frames(chunks) {
                Ok(request) => request,
                Err(_error) => return Ok(Err(SessionRpcError::InvalidRequest)),
            };
            let (session, request, message) = request.into_parts();
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            Ok(control.respond(request, message).await.map_err(Into::into))
        }
    }
}

/// Encodes one logical respond request into transport frames.
pub fn frames_from_respond_request(
    request: &RespondRequest,
) -> Result<Vec<RespondRequestFrame>, AgentWireError> {
    serde_json::to_vec(request)?
        .chunks(FRAME_PAYLOAD_SIZE)
        .map(RespondRequestFrame::new)
        .collect()
}

/// Decodes one logical respond request from transport frames.
pub fn respond_request_from_frames(
    frames: impl IntoIterator<Item = RespondRequestFrame>,
) -> Result<RespondRequest, AgentWireError> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend_from_slice(frame.bytes()?);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

use alloc::vec::Vec;

use barracuda_agent_runtime::{Message, SessionId};
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::wire::{read_payload, write_payload, AgentWireError};

use super::{SessionRegistry, SessionRpcError};

const FRAME_PAYLOAD_SIZE: usize = 126;

/// Logical request corresponding to `SessionControl::append`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AppendRequest {
    session: SessionId,
    message: Message,
}

impl AppendRequest {
    /// Creates an append request.
    #[must_use]
    pub fn new(session: SessionId, message: Message) -> Self {
        Self { session, message }
    }

    pub(crate) fn into_parts(self) -> (SessionId, Message) {
        (self.session, self.message)
    }
}

/// One frame of a variable-length [`AppendRequest`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct AppendRequestFrame {
    length: u16,
    payload: [u8; FRAME_PAYLOAD_SIZE],
}

impl AppendRequestFrame {
    fn new(bytes: &[u8]) -> Result<Self, AgentWireError> {
        let mut payload = [0; FRAME_PAYLOAD_SIZE];
        let length = write_payload(&mut payload, bytes)?;
        Ok(Self { length, payload })
    }

    fn bytes(&self) -> Result<&[u8], AgentWireError> {
        read_payload(&self.payload, self.length)
    }
}

/// RPC corresponding to `SessionControl::append`.
pub struct Append;

impl RpcMethod for Append {
    const ADDRESS: &'static str = "session.append";
    type Request = AppendRequestFrame;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Streaming;
    type Output = Unary;
}

/// Builds the reusable handler for [`Append`].
pub fn append_handler(registry: SessionRegistry) -> impl RpcHandler<Append> {
    move |_context, mut frames: RpcStream<RpcFrame<AppendRequestFrame>>| {
        let registry = registry.clone();
        async move {
            let mut chunks = Vec::new();
            while let Some(frame) = frames.next().await {
                chunks.push(*frame?.view()?);
            }
            let request = match append_request_from_frames(chunks) {
                Ok(request) => request,
                Err(_error) => return Ok(Err(SessionRpcError::InvalidRequest)),
            };
            let (session, message) = request.into_parts();
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            Ok(control.append(message).await.map_err(Into::into))
        }
    }
}

/// Encodes one logical append request into transport frames.
pub fn frames_from_append_request(
    request: &AppendRequest,
) -> Result<Vec<AppendRequestFrame>, AgentWireError> {
    serde_json::to_vec(request)?
        .chunks(FRAME_PAYLOAD_SIZE)
        .map(AppendRequestFrame::new)
        .collect()
}

/// Decodes one logical append request from transport frames.
pub fn append_request_from_frames(
    frames: impl IntoIterator<Item = AppendRequestFrame>,
) -> Result<AppendRequest, AgentWireError> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend_from_slice(frame.bytes()?);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

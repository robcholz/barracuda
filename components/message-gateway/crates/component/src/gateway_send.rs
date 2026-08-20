use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary};
use gateway::{MessageGateway, MessageTarget, SendMessageRequest};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::{route::GatewayRoute, wire::GatewayWireError};

const FRAME_PAYLOAD_SIZE: usize = 62;

/// Gateway-owned logical request accepted by `gateway.send`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct GatewayOutboundMessage {
    /// Destination route and provider conversation identity.
    pub route: GatewayRoute,
    /// Text to deliver.
    pub text: String,
    /// Optional provider message identifier being replied to.
    pub reply_to: Option<String>,
}

/// One fixed-size frame carrying a [`GatewayOutboundMessage`] RPC request.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct GatewaySendRequestFrame {
    length: u16,
    payload: [u8; FRAME_PAYLOAD_SIZE],
}

impl GatewaySendRequestFrame {
    fn new(bytes: &[u8]) -> Result<Self, GatewayWireError> {
        let length = u16::try_from(bytes.len()).map_err(|_| GatewayWireError::InvalidChunk)?;
        if bytes.len() > FRAME_PAYLOAD_SIZE {
            return Err(GatewayWireError::InvalidChunk);
        }
        let mut payload = [0_u8; FRAME_PAYLOAD_SIZE];
        payload
            .get_mut(..bytes.len())
            .ok_or(GatewayWireError::InvalidChunk)?
            .copy_from_slice(bytes);
        Ok(Self { length, payload })
    }

    fn bytes(&self) -> Result<&[u8], GatewayWireError> {
        self.payload
            .get(..usize::from(self.length))
            .ok_or(GatewayWireError::InvalidChunk)
    }
}

/// Business failure returned by `gateway.send`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum GatewaySendError {
    /// The request frame stream or JSON did not encode `GatewayOutboundMessage`.
    InvalidRequest,
    /// Channel selection or provider delivery failed.
    Delivery,
}

/// Outbound message-delivery RPC.
pub struct GatewaySend;

impl RpcMethod for GatewaySend {
    const ADDRESS: &'static str = "gateway.send";
    type Request = GatewaySendRequestFrame;
    type Response = ();
    type Error = GatewaySendError;
    type Input = Streaming;
    type Output = Unary;
}

/// Builds the reusable handler for [`GatewaySend`].
pub fn gateway_send_handler(gateway: Rc<MessageGateway>) -> impl RpcHandler<GatewaySend> {
    move |_context, mut frames: RpcStream<RpcFrame<GatewaySendRequestFrame>>| {
        let gateway = Rc::clone(&gateway);
        async move {
            let mut chunks = Vec::new();
            while let Some(frame) = frames.next().await {
                chunks.push(*frame?.view()?);
            }
            let message = match gateway_send_from_frames(chunks) {
                Ok(message) => message,
                Err(_error) => return Ok(Err(GatewaySendError::InvalidRequest)),
            };
            let mut target =
                MessageTarget::new(message.route.channel, message.route.conversation_id);
            target.thread_id = message.route.thread_id;
            let mut request = SendMessageRequest::text(target, message.text);
            request.reply_to = message.reply_to;
            match gateway.send_message(request).await {
                Ok(_receipt) => Ok(Ok(())),
                Err(_error) => Ok(Err(GatewaySendError::Delivery)),
            }
        }
    }
}

/// Encodes a Gateway send request into frames.
pub fn frames_from_gateway_send(
    value: &GatewayOutboundMessage,
) -> Result<Vec<GatewaySendRequestFrame>, GatewayWireError> {
    serde_json::to_vec(value)?
        .chunks(FRAME_PAYLOAD_SIZE)
        .map(GatewaySendRequestFrame::new)
        .collect()
}

/// Decodes a Gateway send request from frames.
pub fn gateway_send_from_frames(
    frames: impl IntoIterator<Item = GatewaySendRequestFrame>,
) -> Result<GatewayOutboundMessage, GatewayWireError> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend_from_slice(frame.bytes()?);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

use alloc::string::String;
use alloc::vec::Vec;

use barracuda_event_router::{Event, Streaming};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::{route::GatewayRoute, wire::GatewayWireError};

const FRAME_PAYLOAD_SIZE: usize = 62;

/// Gateway-owned logical payload of `gateway.message.received`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct GatewayInboundMessage {
    /// Origin route and conversation identity.
    pub route: GatewayRoute,
    /// Provider-assigned message identifier.
    pub message_id: String,
    /// User-visible message text.
    pub text: String,
}

/// One fixed-size frame carrying a [`GatewayInboundMessage`] Event payload.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct GatewayEventFrame {
    length: u16,
    payload: [u8; FRAME_PAYLOAD_SIZE],
}

impl GatewayEventFrame {
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

/// Event emitted for each normalized inbound text message.
pub struct GatewayMessageReceived;

impl Event for GatewayMessageReceived {
    const ID: &'static str = "gateway.message.received";
    type Message = GatewayEventFrame;
    type Input = Streaming;
}

/// Encodes a Gateway Event into frames.
pub fn frames_from_gateway_event(
    value: &GatewayInboundMessage,
) -> Result<Vec<GatewayEventFrame>, GatewayWireError> {
    serde_json::to_vec(value)?
        .chunks(FRAME_PAYLOAD_SIZE)
        .map(GatewayEventFrame::new)
        .collect()
}

/// Decodes a Gateway Event from frames.
pub fn gateway_event_from_frames(
    frames: impl IntoIterator<Item = GatewayEventFrame>,
) -> Result<GatewayInboundMessage, GatewayWireError> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend_from_slice(frame.bytes()?);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

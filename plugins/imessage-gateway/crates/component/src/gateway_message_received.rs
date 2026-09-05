use alloc::string::String;
use core::fmt;

use barracuda_event_router::{Event, EventEmitter, JsonPayload, RpcError};
use serde::{Deserialize, Serialize};

use crate::json::{
    bounded_json_prefix, encoded_json_len, event_input_capacity, valid_required,
    write_encoded_json, write_json_string, EncodedJson,
};
use crate::route::GatewayRoute;

/// Gateway-owned logical inbound message published by channel providers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GatewayInboundMessage {
    /// Origin route and conversation identity.
    pub route: GatewayRoute,
    /// Provider-assigned message identifier.
    pub message_id: String,
    /// User-visible message text.
    pub text: String,
}

/// Bounded JSON Event emitted for normalized inbound message data.
pub struct GatewayMessageReceived;

impl Event for GatewayMessageReceived {
    const ID: &'static str = "gateway.message.received";
}

pub(crate) fn validate_inbound(message: &GatewayInboundMessage, event_input_bytes: usize) -> bool {
    valid_required(&message.route.channel)
        && valid_required(&message.route.conversation_id)
        && valid_required(&message.message_id)
        && InboundEvent::start(1, message)
            .encoded_len()
            .is_ok_and(|length| length <= event_input_bytes)
}

pub(crate) async fn emit_inbound<const M: usize>(
    emitter: &EventEmitter<M>,
    stream_id: u64,
    message: &GatewayInboundMessage,
) -> Result<(), barracuda_event_router::EmitError> {
    let input_capacity = event_input_capacity::<M>(GatewayMessageReceived::ID)?;
    let complete = InboundEvent::complete(stream_id, message);
    if complete
        .encoded_len()
        .is_ok_and(|length| length <= input_capacity)
    {
        return emitter.emit::<GatewayMessageReceived>(&complete).await;
    }

    emitter
        .emit::<GatewayMessageReceived>(&InboundEvent::start(stream_id, message))
        .await?;
    let mut remaining = message.text.as_str();
    let mut sequence = 1_u32;
    while !remaining.is_empty() {
        let empty = InboundEvent::chunk(stream_id, sequence, "").encoded_len()?;
        let available = input_capacity
            .checked_sub(empty)
            .ok_or(RpcError::FrameTooLarge {
                size: empty,
                capacity: input_capacity,
            })?;
        let chunk = bounded_json_prefix(remaining, available);
        if chunk.is_empty() {
            return Err(barracuda_event_router::EmitError::Rpc(
                RpcError::InvalidFrameState,
            ));
        }
        emitter
            .emit::<GatewayMessageReceived>(&InboundEvent::chunk(stream_id, sequence, chunk))
            .await?;
        remaining = remaining
            .get(chunk.len()..)
            .ok_or(barracuda_event_router::EmitError::Rpc(
                RpcError::InvalidFrameState,
            ))?;
        sequence = sequence.saturating_add(1);
    }
    emitter
        .emit::<GatewayMessageReceived>(&InboundEvent::finish(stream_id, sequence))
        .await
}

enum InboundPhase<'a> {
    Complete(&'a GatewayInboundMessage),
    Start(&'a GatewayInboundMessage),
    Chunk(&'a str),
    Finish,
}

struct InboundEvent<'a> {
    stream_id: u64,
    sequence: u32,
    phase: InboundPhase<'a>,
}

impl<'a> InboundEvent<'a> {
    const fn complete(stream_id: u64, message: &'a GatewayInboundMessage) -> Self {
        Self {
            stream_id,
            sequence: 0,
            phase: InboundPhase::Complete(message),
        }
    }

    const fn start(stream_id: u64, message: &'a GatewayInboundMessage) -> Self {
        Self {
            stream_id,
            sequence: 0,
            phase: InboundPhase::Start(message),
        }
    }

    const fn chunk(stream_id: u64, sequence: u32, text: &'a str) -> Self {
        Self {
            stream_id,
            sequence,
            phase: InboundPhase::Chunk(text),
        }
    }

    const fn finish(stream_id: u64, sequence: u32) -> Self {
        Self {
            stream_id,
            sequence,
            phase: InboundPhase::Finish,
        }
    }

    fn write_route(writer: &mut impl fmt::Write, message: &GatewayInboundMessage) -> fmt::Result {
        writer.write_str(",\"route\":{\"channel\":")?;
        write_json_string(writer, &message.route.channel)?;
        writer.write_str(",\"conversation_id\":")?;
        write_json_string(writer, &message.route.conversation_id)?;
        if let Some(thread_id) = &message.route.thread_id {
            writer.write_str(",\"thread_id\":")?;
            write_json_string(writer, thread_id)?;
        }
        writer.write_str("},\"message_id\":")?;
        write_json_string(writer, &message.message_id)
    }
}

impl EncodedJson for InboundEvent<'_> {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        write!(
            writer,
            "{{\"stream_id\":{},\"sequence\":{},",
            self.stream_id, self.sequence
        )?;
        match self.phase {
            InboundPhase::Complete(message) => {
                writer.write_str("\"phase\":\"complete\",\"terminal\":true")?;
                Self::write_route(writer, message)?;
                writer.write_str(",\"text\":")?;
                write_json_string(writer, &message.text)?;
            }
            InboundPhase::Start(message) => {
                writer.write_str("\"phase\":\"start\",\"terminal\":false")?;
                Self::write_route(writer, message)?;
            }
            InboundPhase::Chunk(text) => {
                writer.write_str("\"phase\":\"chunk\",\"terminal\":false,\"text\":")?;
                write_json_string(writer, text)?;
            }
            InboundPhase::Finish => {
                writer.write_str("\"phase\":\"finish\",\"terminal\":true")?;
            }
        }
        writer.write_char('}')
    }
}

impl JsonPayload for InboundEvent<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        encoded_json_len(self)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        self.encoded_len()?;
        write_encoded_json(self, destination)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::string::String;

    use barracuda_event_router::{Event, JsonPayload};

    use super::{GatewayMessageReceived, InboundEvent};
    use crate::json::{bounded_json_prefix, event_input_capacity};

    #[test]
    fn inbound_chunk_uses_the_actual_event_lane_budget() {
        let input_capacity = event_input_capacity::<512>(GatewayMessageReceived::ID)
            .expect("512-byte Event lane fits its envelope");
        let empty = InboundEvent::chunk(1, 1, "")
            .encoded_len()
            .expect("empty chunk length");
        let available = input_capacity - empty;
        let text = String::from("😀\\\"").repeat(100);
        let chunk = bounded_json_prefix(&text, available);
        let payload = InboundEvent::chunk(1, 1, chunk);

        assert!(chunk.len() > 240);
        assert!(payload.encoded_len().expect("chunk length") <= input_capacity);
        assert!(text.is_char_boundary(chunk.len()));
    }
}

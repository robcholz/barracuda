use alloc::string::String;
use core::fmt;

use barracuda_event_router::{Event, EventEmitter, JsonPayload, RpcError};
use serde::{Deserialize, Serialize};

use crate::json::{
    encoded_json_len, valid_required, write_encoded_json, write_json_string, EncodedJson,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InboundValidationError {
    InvalidMessage,
    MessageTooLarge,
}

pub(crate) fn validate_inbound(
    message: &GatewayInboundMessage,
    event_input_bytes: usize,
) -> Result<(), InboundValidationError> {
    if !valid_required(&message.route.channel)
        || !valid_required(&message.route.conversation_id)
        || !valid_required(&message.message_id)
    {
        return Err(InboundValidationError::InvalidMessage);
    }

    match message.encoded_len() {
        Ok(length) if length <= event_input_bytes => Ok(()),
        Ok(_) | Err(_) => Err(InboundValidationError::MessageTooLarge),
    }
}

pub(crate) async fn emit_inbound<const M: usize>(
    emitter: &EventEmitter<M>,
    message: &GatewayInboundMessage,
) -> Result<(), barracuda_event_router::EmitError> {
    emitter.emit::<GatewayMessageReceived>(message).await
}

impl EncodedJson for GatewayInboundMessage {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        writer.write_str("{\"route\":{\"channel\":")?;
        write_json_string(writer, &self.route.channel)?;
        writer.write_str(",\"conversation_id\":")?;
        write_json_string(writer, &self.route.conversation_id)?;
        if let Some(thread_id) = &self.route.thread_id {
            writer.write_str(",\"thread_id\":")?;
            write_json_string(writer, thread_id)?;
        }
        writer.write_str("},\"message_id\":")?;
        write_json_string(writer, &self.message_id)?;
        writer.write_str(",\"text\":")?;
        write_json_string(writer, &self.text)?;
        writer.write_char('}')
    }
}

impl JsonPayload for GatewayInboundMessage {
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

    use barracuda_event_router::JsonPayload;

    use super::{validate_inbound, GatewayInboundMessage, InboundValidationError};
    use crate::route::GatewayRoute;

    #[test]
    fn inbound_event_is_one_complete_natural_document() {
        let message = GatewayInboundMessage {
            route: GatewayRoute::new("test", "chat").with_thread("topic"),
            message_id: "message-1".into(),
            text: "hello".into(),
        };
        let mut encoded = [0_u8; 256];
        let length = message.write_json(&mut encoded).expect("encode message");

        assert_eq!(
            core::str::from_utf8(encoded.get(..length).expect("encoded range"))
                .expect("UTF-8 JSON"),
            r#"{"route":{"channel":"test","conversation_id":"chat","thread_id":"topic"},"message_id":"message-1","text":"hello"}"#
        );
    }

    #[test]
    fn inbound_validation_uses_the_complete_encoded_document_boundary() {
        let message = GatewayInboundMessage {
            route: GatewayRoute::new("test", "chat"),
            message_id: "message-1".into(),
            text: "hello".into(),
        };
        let encoded_length = message.encoded_len().expect("encoded message length");

        assert_eq!(validate_inbound(&message, encoded_length), Ok(()));
        assert_eq!(
            validate_inbound(
                &message,
                encoded_length.checked_sub(1).expect("non-empty document"),
            ),
            Err(InboundValidationError::MessageTooLarge)
        );
    }
}

use alloc::{rc::Rc, string::String};
use core::fmt;

use barracuda_event_router::{
    json_schema, JsonHandler, JsonPayload, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RpcError,
};
use gateway::{ChannelError, GatewayError, MessageGateway, MessageTarget, SendMessageRequest};
use serde::Deserialize;

use crate::json::{
    encoded_json_len, valid_required, write_encoded_json, write_json_string, EncodedJson,
    ErrorResponse, GatewayJsonError, FRAME_CAPACITY,
};

/// Sends one bounded complete text message.
pub struct GatewaySend;

impl JsonRpcSchema for GatewaySend {
    const ADDRESS: &'static str = "gateway.send";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("send", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("send", response);
    const MAX_REQUEST_BYTES: usize = FRAME_CAPACITY;
    const MAX_RESPONSE_BYTES: usize = FRAME_CAPACITY;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendRequest<'a> {
    #[serde(borrow)]
    channel: &'a str,
    #[serde(borrow)]
    conversation_id: &'a str,
    #[serde(default, borrow)]
    thread_id: Option<&'a str>,
    #[serde(default, borrow)]
    reply_to: Option<&'a str>,
    #[serde(borrow)]
    text: &'a str,
}

impl SendRequest<'_> {
    fn is_valid(&self) -> bool {
        valid_required(self.channel) && valid_required(self.conversation_id)
    }
}

struct ReceiptResponse<'a> {
    message_id: &'a str,
}

impl EncodedJson for ReceiptResponse<'_> {
    fn encode(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        writer.write_str("{\"message_id\":")?;
        write_json_string(writer, self.message_id)?;
        writer.write_char('}')
    }
}

impl JsonPayload for ReceiptResponse<'_> {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        encoded_json_len(self)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        write_encoded_json(self, destination)
    }
}

pub(crate) fn map_gateway_error(error: &GatewayError) -> GatewayJsonError {
    match error {
        GatewayError::UnknownChannel { .. } => GatewayJsonError::UnknownChannel,
        GatewayError::DuplicateChannel { .. } => GatewayJsonError::Delivery,
        GatewayError::Channel { source, .. } => match source {
            ChannelError::Unsupported { .. } => GatewayJsonError::Unsupported,
            ChannelError::InvalidRequest { .. } => GatewayJsonError::InvalidRequest,
            ChannelError::Authentication => GatewayJsonError::Authentication,
            ChannelError::RateLimited => GatewayJsonError::RateLimited,
            ChannelError::Transport { .. }
            | ChannelError::Platform { .. }
            | ChannelError::Stream(_) => GatewayJsonError::Delivery,
        },
    }
}

/// Builds the lane-backed JSON handler for [`GatewaySend`].
pub fn gateway_send_handler(gateway: Rc<MessageGateway>) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let gateway = Rc::clone(&gateway);
        async move {
            let request = request.deserialize::<SendRequest<'_>>()?;
            if !request.is_valid() {
                return response
                    .write(&ErrorResponse(GatewayJsonError::InvalidRequest))
                    .await;
            }

            let mut target = MessageTarget::new(request.channel, request.conversation_id);
            target.thread_id = request.thread_id.map(String::from);
            let mut outbound = SendMessageRequest::text(target, request.text);
            outbound.reply_to = request.reply_to.map(String::from);
            match gateway.send_message(outbound).await {
                Ok(receipt) => {
                    let receipt = ReceiptResponse {
                        message_id: &receipt.message_id,
                    };
                    if receipt.encoded_len()? > GatewaySend::MAX_RESPONSE_BYTES {
                        response
                            .write(&ErrorResponse(GatewayJsonError::InvalidReceipt))
                            .await
                    } else {
                        response.write(&receipt).await
                    }
                }
                Err(error) => {
                    response
                        .write(&ErrorResponse(map_gateway_error(&error)))
                        .await
                }
            }
        }
    }
}

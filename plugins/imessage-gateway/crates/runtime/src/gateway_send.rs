use alloc::string::String;

use gateway::{MessageGateway, MessageTarget, SendMessageRequest};
use serde::{Deserialize, Serialize};

use crate::json::{map_gateway_error, valid_required, GatewayOperationError};

/// Request to send one complete text message.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewaySendRequest {
    /// Registered provider channel.
    pub channel: String,
    /// Provider conversation identifier.
    pub conversation_id: String,
    /// Optional provider thread identifier.
    pub thread_id: Option<String>,
    /// Optional provider message being replied to.
    pub reply_to: Option<String>,
    /// Complete user-visible text.
    pub text: String,
}

/// Provider receipt for one sent message.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GatewaySendResponse {
    /// Provider-assigned message identifier.
    pub message_id: String,
}

pub(crate) async fn send(
    gateway: &MessageGateway,
    request: GatewaySendRequest,
) -> Result<GatewaySendResponse, GatewayOperationError> {
    if !valid_required(&request.channel) || !valid_required(&request.conversation_id) {
        return Err(GatewayOperationError::InvalidRequest);
    }
    let mut target = MessageTarget::new(request.channel, request.conversation_id);
    target.thread_id = request.thread_id;
    let mut outbound = SendMessageRequest::text(target, request.text);
    outbound.reply_to = request.reply_to;
    gateway
        .send_message(outbound)
        .await
        .map(|receipt| GatewaySendResponse {
            message_id: receipt.message_id,
        })
        .map_err(|error| map_gateway_error(&error))
}

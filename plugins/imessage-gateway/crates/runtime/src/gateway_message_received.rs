use alloc::string::String;

use barracuda_workflow_plugin::Event;
use serde::{Deserialize, Serialize};

use crate::json::valid_required;
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

/// Workflow Event emitted for normalized inbound message data.
pub struct GatewayMessageReceived;

impl Event for GatewayMessageReceived {
    const ID: &'static str = "gateway.message.received";
}

pub(crate) fn valid_inbound(message: &GatewayInboundMessage) -> bool {
    valid_required(&message.route.channel)
        && valid_required(&message.route.conversation_id)
        && valid_required(&message.message_id)
}

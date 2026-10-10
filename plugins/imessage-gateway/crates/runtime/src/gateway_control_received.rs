use barracuda_workflow_plugin::Event;
use serde::{Deserialize, Serialize};

use crate::json::valid_required;
use crate::route::GatewayRoute;

/// What a user asked of the turn running for their conversation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayControlKind {
    /// Stop at the turn's next iteration boundary, keeping what it produced.
    Interrupt,
    /// Abort the turn at once.
    Cancel,
}

/// Gateway-owned control request published by channel providers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GatewayInboundControl {
    /// Origin route and conversation identity.
    pub route: GatewayRoute,
    /// The requested control.
    pub control: GatewayControlKind,
}

/// Workflow Event emitted for a normalized inbound control request.
pub struct GatewayControlReceived;

impl Event for GatewayControlReceived {
    const ID: &'static str = "gateway.control.received";
}

pub(crate) fn valid_control(control: &GatewayInboundControl) -> bool {
    valid_required(&control.route.channel) && valid_required(&control.route.conversation_id)
}

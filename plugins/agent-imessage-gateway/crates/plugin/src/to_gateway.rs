use alloc::{boxed::Box, rc::Rc, string::String};

use barracuda_plugin::manager::PluginStorage;
use barracuda_workflow_plugin::{
    WorkflowActionFuture, WorkflowActionHandler, WorkflowActionSchema, workflow_action_schema,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    bridge::{BridgeControl, BridgeShared},
    state::{BridgeError, GatewayEvent, GatewayTarget},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToGatewayRequest {
    session: String,
    sequence: u64,
    #[serde(rename = "type")]
    event_type: String,
    payload: Value,
}

#[derive(Deserialize)]
struct TurnStartedPayload {
    turn: String,
    origin: String,
}

#[derive(Serialize)]
pub(crate) struct RouteResponse {
    channel: String,
    conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum ToGatewayResponse {
    Missing {},
    Target {
        route: RouteResponse,
        reply_to: Option<String>,
    },
    Error {
        error: BridgeError,
    },
}

fn gateway_event(request: &ToGatewayRequest) -> Result<GatewayEvent, BridgeError> {
    match request.event_type.as_str() {
        "turn_started" => {
            let payload = serde_json::from_value::<TurnStartedPayload>(request.payload.clone())
                .map_err(|_error| BridgeError::InvalidRequest)?;
            if payload.turn.is_empty() {
                return Err(BridgeError::InvalidRequest);
            }
            match payload.origin.as_str() {
                "user" => Ok(GatewayEvent::UserTurnStarted),
                "tool_call" => Ok(GatewayEvent::OtherTurnStarted),
                _ => Err(BridgeError::InvalidRequest),
            }
        }
        "turn_ended" => Ok(GatewayEvent::TurnEnded),
        "closed" => Ok(GatewayEvent::Closed),
        _ => Ok(GatewayEvent::Continuing),
    }
}

fn response(target: Option<GatewayTarget>) -> ToGatewayResponse {
    match target {
        Some(target) => ToGatewayResponse::Target {
            route: RouteResponse {
                channel: target.route.channel,
                conversation_id: target.route.conversation_id,
                thread_id: target.route.thread_id,
            },
            reply_to: target.reply_to,
        },
        None => ToGatewayResponse::Missing {},
    }
}

async fn invoke<Storage>(
    shared: &BridgeShared<Storage>,
    request: ToGatewayRequest,
) -> ToGatewayResponse
where
    Storage: PluginStorage,
{
    log::debug!(
        "IMessage Bridge received Agent event `{}` for `{}` at sequence {}",
        request.event_type,
        request.session,
        request.sequence
    );
    let result = match gateway_event(&request) {
        Ok(event) => shared
            .book
            .lock()
            .await
            .gateway_target(&request.session, event),
        Err(error) => Err(error),
    };
    match &result {
        Ok(Some(target)) if request.event_type == "turn_started" => log::info!(
            "IMessage Bridge routed Agent turn `{}` to `{}` conversation `{}`",
            request.session,
            target.route.channel,
            target.route.conversation_id
        ),
        Ok(Some(_target)) if request.event_type == "turn_ended" => log::info!(
            "IMessage Bridge routed terminal turn event for `{}` at sequence {}",
            request.session,
            request.sequence
        ),
        Ok(Some(_target)) => log::debug!(
            "IMessage Bridge routed `{}` for `{}` at sequence {}",
            request.event_type,
            request.session,
            request.sequence
        ),
        Ok(None) if request.event_type == "closed" => log::info!(
            "IMessage Bridge marked Agent session `{}` closed",
            request.session
        ),
        Ok(None) => log::warn!(
            "IMessage Bridge has no Gateway route for Agent session `{}` event `{}`",
            request.session,
            request.event_type
        ),
        Err(error) => log::warn!(
            "IMessage Bridge rejected Agent event `{}` for `{}`: {}",
            request.event_type,
            request.session,
            error.code()
        ),
    }
    match result {
        Ok(target) => response(target),
        Err(error) => ToGatewayResponse::Error { error },
    }
}

pub(crate) struct ToGatewayAction<Storage> {
    control: BridgeControl<Storage>,
}

impl<Storage> ToGatewayAction<Storage> {
    pub(crate) const fn new(control: BridgeControl<Storage>) -> Self {
        Self { control }
    }
}

impl<Storage> WorkflowActionHandler for ToGatewayAction<Storage>
where
    Storage: PluginStorage,
{
    type Request = ToGatewayRequest;
    type Response = ToGatewayResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("imessage_bridge.to_gateway");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let shared = Rc::clone(&self.control.shared);
        Box::pin(async move { Ok(invoke(&shared, request).await) })
    }
}

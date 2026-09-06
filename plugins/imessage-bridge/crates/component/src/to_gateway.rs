use alloc::rc::Rc;

use barracuda_event_router::{
    JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, json_schema,
};
use barracuda_plugin::manager::PluginStorage;
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::{
    component::BridgeControl,
    json::{error_response, gateway_target_response},
    state::{BridgeError, GatewayEvent},
};

/// Resolves the Gateway delivery target for one complete Agent session Event.
pub struct ToGateway;

impl JsonRpcSchema for ToGateway {
    const ADDRESS: &'static str = "imessage_bridge.to_gateway";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("to_gateway", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("to_gateway", response);
    const MAX_REQUEST_BYTES: usize = 512;
    const MAX_RESPONSE_BYTES: usize = 512;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToGatewayRequest<'a> {
    #[serde(borrow)]
    session: &'a str,
    sequence: u64,
    #[serde(rename = "type", borrow)]
    event_type: &'a str,
    #[serde(borrow)]
    payload: &'a RawValue,
}

#[derive(Deserialize)]
struct TurnStartedPayload<'a> {
    #[serde(borrow)]
    turn: &'a str,
    #[serde(borrow)]
    origin: &'a str,
}

fn gateway_event(request: &ToGatewayRequest<'_>) -> Result<GatewayEvent, BridgeError> {
    match request.event_type {
        "turn_started" => {
            let payload = serde_json::from_str::<TurnStartedPayload<'_>>(request.payload.get())
                .map_err(|_error| BridgeError::InvalidRequest)?;
            if payload.turn.is_empty() {
                return Err(BridgeError::InvalidRequest);
            }
            match payload.origin {
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

pub(crate) fn to_gateway_handler<Storage>(control: BridgeControl<Storage>) -> impl JsonHandler
where
    Storage: PluginStorage,
{
    move |_context, request: JsonRef, response: JsonWriter| {
        let shared = Rc::clone(&control.shared);
        async move {
            let request = request.deserialize::<ToGatewayRequest<'_>>()?;
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
                    .gateway_target(request.session, event),
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
            let result = result.map(gateway_target_response);
            match result {
                Ok(value) => response.write(&value).await,
                Err(error) => response.write(&error_response(error)).await,
            }
        }
    }
}

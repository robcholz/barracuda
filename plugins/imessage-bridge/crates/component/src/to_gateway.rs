use alloc::rc::Rc;

use barracuda_event_router::{
    JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, json_schema,
};
use barracuda_plugin_manager::PluginStorage;
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::{
    component::BridgeControl,
    json::{error_response, gateway_target_response},
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
    #[serde(rename = "sequence")]
    _sequence: u64,
    #[serde(rename = "type", borrow)]
    event_type: &'a str,
    #[serde(rename = "payload", borrow)]
    _payload: &'a RawValue,
}

pub(crate) fn to_gateway_handler<Storage>(control: BridgeControl<Storage>) -> impl JsonHandler
where
    Storage: PluginStorage,
{
    move |_context, request: JsonRef, response: JsonWriter| {
        let shared = Rc::clone(&control.shared);
        async move {
            let request = request.deserialize::<ToGatewayRequest<'_>>()?;
            let result = shared
                .book
                .lock()
                .await
                .gateway_target(request.session, request.event_type == "closed")
                .map(gateway_target_response);
            match result {
                Ok(value) => response.write(&value).await,
                Err(error) => response.write(&error_response(error)).await,
            }
        }
    }
}

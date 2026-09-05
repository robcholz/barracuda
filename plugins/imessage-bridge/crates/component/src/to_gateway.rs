use alloc::rc::Rc;

use barracuda_event_router::{
    JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, json_schema,
};
use barracuda_plugin_manager::PluginStorage;
use serde::Deserialize;

use crate::{
    component::BridgeControl,
    json::{command_response, error_response, forwarding_response},
    state::SessionField,
};

/// Converts Agent session Event chunks into one-shot Gateway stream commands.
pub struct ToGateway;

impl JsonRpcSchema for ToGateway {
    const ADDRESS: &'static str = "imessage_bridge.to_gateway";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("to_gateway", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("to_gateway", response);
    const MAX_REQUEST_BYTES: usize = 512;
    const MAX_RESPONSE_BYTES: usize = 512;
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ToGatewayRequest<'a> {
    Event {
        #[serde(borrow)]
        session: &'a str,
        #[serde(borrow)]
        run: &'a str,
        sequence: u64,
        chunk_index: u64,
        #[serde(borrow)]
        field: &'a str,
        #[serde(borrow)]
        chunk: &'a str,
        field_complete: bool,
        event_complete: bool,
        #[serde(default, borrow)]
        terminal: Option<&'a str>,
    },
    Command {
        #[serde(borrow)]
        command_id: &'a str,
    },
}

pub(crate) fn to_gateway_handler<Storage>(control: BridgeControl<Storage>) -> impl JsonHandler
where
    Storage: PluginStorage,
{
    move |_context, request: JsonRef, response: JsonWriter| {
        let shared = Rc::clone(&control.shared);
        async move {
            let request = request.deserialize::<ToGatewayRequest<'_>>()?;
            let result = match request {
                ToGatewayRequest::Event {
                    session,
                    run,
                    sequence,
                    chunk_index,
                    field,
                    chunk,
                    field_complete,
                    event_complete,
                    terminal,
                } => {
                    let _bounded_order = (chunk_index, event_complete);
                    shared
                        .book
                        .lock()
                        .await
                        .process_field(SessionField {
                            session,
                            run,
                            sequence,
                            field,
                            chunk,
                            field_complete,
                            terminal,
                        })
                        .map(|command_id| forwarding_response(command_id.as_deref()))
                }
                ToGatewayRequest::Command { command_id } => shared
                    .book
                    .lock()
                    .await
                    .take_command(command_id)
                    .map(command_response),
            };
            match result {
                Ok(value) => response.write(&value).await,
                Err(error) => response.write(&error_response(error)).await,
            }
        }
    }
}

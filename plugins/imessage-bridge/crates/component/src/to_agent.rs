use alloc::rc::Rc;

use barracuda_event_router::{
    JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, json_schema,
};
use barracuda_plugin_manager::PluginStorage;
use serde::Deserialize;

use crate::{
    component::BridgeControl,
    json::{bound_response, error_response, resolve_response},
    state::{BridgeError, Route, persist_mapping},
};

/// Resolves or binds a Gateway route to an Agent session.
pub struct ToAgent;

impl JsonRpcSchema for ToAgent {
    const ADDRESS: &'static str = "imessage_bridge.to_agent";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("to_agent", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("to_agent", response);
    const MAX_REQUEST_BYTES: usize = 512;
    const MAX_RESPONSE_BYTES: usize = 128;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteRequest<'a> {
    #[serde(borrow)]
    channel: &'a str,
    #[serde(borrow)]
    conversation_id: &'a str,
    #[serde(default, borrow)]
    thread_id: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ToAgentRequest<'a> {
    Resolve {
        #[serde(borrow)]
        route: RouteRequest<'a>,
        #[serde(borrow)]
        message_id: &'a str,
        #[serde(borrow)]
        text: &'a str,
    },
    Bind {
        #[serde(borrow)]
        route: RouteRequest<'a>,
        #[serde(borrow)]
        message_id: &'a str,
        #[serde(borrow)]
        session: &'a str,
    },
}

fn route(request: RouteRequest<'_>) -> Result<Route, BridgeError> {
    Route::new(request.channel, request.conversation_id, request.thread_id)
}

pub(crate) fn to_agent_handler<Storage>(control: BridgeControl<Storage>) -> impl JsonHandler
where
    Storage: PluginStorage,
{
    move |_context, request: JsonRef, response: JsonWriter| {
        let shared = Rc::clone(&control.shared);
        async move {
            let request = request.deserialize::<ToAgentRequest<'_>>()?;
            let result = match request {
                ToAgentRequest::Resolve {
                    route: route_request,
                    message_id,
                    text,
                } => {
                    let _complete_message = text;
                    match route(route_request) {
                        Ok(route) => {
                            let mut book = shared.book.lock().await;
                            match book.resolve(&route) {
                                crate::state::ResolveResult::Missing => {
                                    Ok(resolve_response(crate::state::ResolveResult::Missing))
                                }
                                found => match book.mapping_with_reply(&route, message_id) {
                                    Ok(mapping) => {
                                        if let Err(error) =
                                            persist_mapping(&shared.storage, &mapping).await
                                        {
                                            Err(error)
                                        } else {
                                            book.commit_mapping(mapping);
                                            Ok(resolve_response(found))
                                        }
                                    }
                                    Err(error) => Err(error),
                                },
                            }
                        }
                        Err(error) => Err(error),
                    }
                }
                ToAgentRequest::Bind {
                    route: route_request,
                    message_id,
                    session,
                } => match route(route_request) {
                    Ok(route) => {
                        let mut book = shared.book.lock().await;
                        match book.prepare_binding(route, message_id, session) {
                            Ok(mapping) => {
                                if let Err(error) = persist_mapping(&shared.storage, &mapping).await
                                {
                                    Err(error)
                                } else {
                                    book.commit_mapping(mapping);
                                    Ok(bound_response(session))
                                }
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Err(error) => Err(error),
                },
            };
            match result {
                Ok(value) => response.write(&value).await,
                Err(error) => response.write(&error_response(error)).await,
            }
        }
    }
}

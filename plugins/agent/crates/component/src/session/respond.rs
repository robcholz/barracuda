use barracuda_agent_runtime::Message;
use barracuda_event_router::{
    json_schema, JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
};
use serde::Deserialize;

use crate::json::{
    map_control_error, parse_input_request, parse_session, AgentRpcError, ErrorResponse,
};

use super::SessionRegistry;

/// Answers one pending input request on an open session.
pub struct Respond;

impl JsonRpcSchema for Respond {
    const ADDRESS: &'static str = "session.respond";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("respond", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("respond", response);
    const MAX_REQUEST_BYTES: usize = 512;
    const MAX_RESPONSE_BYTES: usize = 34;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RespondRequest<'a> {
    #[serde(borrow)]
    session: &'a str,
    #[serde(borrow)]
    request: &'a str,
    #[serde(borrow)]
    text: &'a str,
}

/// Builds the JSON handler for [`Respond`].
pub fn respond_handler(registry: SessionRegistry) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let registry = registry.clone();
        async move {
            let request = request.deserialize::<RespondRequest<'_>>()?;
            let session = match parse_session(request.session) {
                Ok(session) => session,
                Err(error) => return response.write(&ErrorResponse(error)).await,
            };
            let input = match parse_input_request(request.request) {
                Ok(input) => input,
                Err(error) => return response.write(&ErrorResponse(error)).await,
            };
            let Some(control) = registry.get(session) else {
                return response
                    .write(&ErrorResponse(AgentRpcError::SessionNotOpen))
                    .await;
            };

            // SessionControl owns the message after this lane is released.
            match control.respond(input, Message::text(request.text)).await {
                Ok(()) => response.write("{}").await,
                Err(error) => {
                    response
                        .write(&ErrorResponse(map_control_error(error)))
                        .await
                }
            }
        }
    }
}

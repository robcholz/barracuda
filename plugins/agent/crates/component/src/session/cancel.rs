use barracuda_event_router::{
    json_schema, JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
};
use serde::Deserialize;

use crate::json::{map_control_error, parse_session, AgentRpcError, ErrorResponse};

use super::SessionRegistry;

/// Cancels all active work of an open session.
pub struct Cancel;

impl JsonRpcSchema for Cancel {
    const ADDRESS: &'static str = "session.cancel";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("cancel", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("cancel", response);
    const MAX_REQUEST_BYTES: usize = 48;
    const MAX_RESPONSE_BYTES: usize = 34;
}

/// Builds the JSON handler for [`Cancel`].
pub fn cancel_handler(registry: SessionRegistry) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let registry = registry.clone();
        async move {
            let request = request.deserialize::<Request<'_>>()?;
            let session = match parse_session(request.session) {
                Ok(session) => session,
                Err(error) => return response.write(&ErrorResponse(error)).await,
            };
            let Some(control) = registry.get(session) else {
                return response
                    .write(&ErrorResponse(AgentRpcError::SessionNotOpen))
                    .await;
            };
            match control.cancel().await {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request<'a> {
    #[serde(borrow)]
    session: &'a str,
}

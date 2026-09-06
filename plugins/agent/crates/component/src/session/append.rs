use barracuda_agent_runtime::Message;
use barracuda_event_router::{
    json_schema, JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
};
use serde::Deserialize;

use crate::json::{map_control_error, parse_session, AgentRpcError, ErrorResponse};

use super::SessionRegistry;

/// Appends one complete user message to an open session.
pub struct Append;

impl JsonRpcSchema for Append {
    const ADDRESS: &'static str = "session.append";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("append", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("append", response);
    const MAX_REQUEST_BYTES: usize = 512;
    const MAX_RESPONSE_BYTES: usize = 34;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppendRequest<'a> {
    #[serde(borrow)]
    session: &'a str,
    #[serde(borrow)]
    text: &'a str,
}

/// Builds the JSON handler for [`Append`].
pub fn append_handler(registry: SessionRegistry) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let registry = registry.clone();
        async move {
            let request = request.deserialize::<AppendRequest<'_>>()?;
            let session = match parse_session(request.session) {
                Ok(session) => session,
                Err(error) => {
                    log::warn!(
                        "Agent rejected `session.append` for `{}`: {error}",
                        request.session
                    );
                    return response.write(&ErrorResponse(error)).await;
                }
            };
            let Some(control) = registry.get(session) else {
                log::warn!("Agent rejected append for unopened session `{session}`");
                return response
                    .write(&ErrorResponse(AgentRpcError::SessionNotOpen))
                    .await;
            };

            // SessionControl owns the message after this lane is released.
            log::info!(
                "Agent appending {}-byte user message to session `{session}`",
                request.text.len()
            );
            match control.append(Message::text(request.text)).await {
                Ok(()) => {
                    log::info!("Agent accepted user message for session `{session}`");
                    response.write("{}").await
                }
                Err(error) => {
                    let error = map_control_error(error);
                    log::warn!("Agent rejected user message for session `{session}`: {error}");
                    response.write(&ErrorResponse(error)).await
                }
            }
        }
    }
}

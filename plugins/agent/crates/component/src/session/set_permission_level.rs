use barracuda_agent_runtime::PermissionLevel;
use barracuda_event_router::{
    json_schema, JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
};
use serde::Deserialize;

use crate::json::{map_control_error, parse_session, AgentRpcError, ErrorResponse};

use super::SessionRegistry;

/// Changes the permission level used by an open session.
pub struct SetPermissionLevel;

impl JsonRpcSchema for SetPermissionLevel {
    const ADDRESS: &'static str = "session.set_permission_level";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("set_permission_level", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("set_permission_level", response);
    const MAX_REQUEST_BYTES: usize = 80;
    const MAX_RESPONSE_BYTES: usize = 34;
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Level {
    Deny,
    Ask,
    AllowAll,
}

impl From<Level> for PermissionLevel {
    fn from(value: Level) -> Self {
        match value {
            Level::Deny => Self::Deny,
            Level::Ask => Self::Ask,
            Level::AllowAll => Self::AllowAll,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request<'a> {
    #[serde(borrow)]
    session: &'a str,
    level: Level,
}

/// Builds the JSON handler for [`SetPermissionLevel`].
pub fn set_permission_level_handler(registry: SessionRegistry) -> impl JsonHandler {
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
            match control.set_permission_level(request.level.into()).await {
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

use barracuda_agent_runtime::ReasoningEffort;
use barracuda_event_router::{
    json_schema, JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
};
use serde::Deserialize;

use crate::json::{map_control_error, parse_session, AgentRpcError, ErrorResponse};

use super::SessionRegistry;

/// Changes the reasoning effort used by an open session.
pub struct SetReasoningEffort;

impl JsonRpcSchema for SetReasoningEffort {
    const ADDRESS: &'static str = "session.set_reasoning_effort";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("set_reasoning_effort", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("set_reasoning_effort", response);
    const MAX_REQUEST_BYTES: usize = 80;
    const MAX_RESPONSE_BYTES: usize = 34;
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Effort {
    Low,
    Medium,
    High,
    Ultra,
}

impl From<Effort> for ReasoningEffort {
    fn from(value: Effort) -> Self {
        match value {
            Effort::Low => Self::Low,
            Effort::Medium => Self::Medium,
            Effort::High => Self::High,
            Effort::Ultra => Self::Ultra,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request<'a> {
    #[serde(borrow)]
    session: &'a str,
    effort: Effort,
}

/// Builds the JSON handler for [`SetReasoningEffort`].
pub fn set_reasoning_effort_handler(registry: SessionRegistry) -> impl JsonHandler {
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
            match control.set_reasoning_effort(request.effort.into()).await {
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

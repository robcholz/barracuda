use alloc::rc::Rc;

use barracuda_agent_runtime::{AgentRuntime, RuntimeError, SessionCreateError, SessionPersistence};
use barracuda_event_router::{
    json_schema, JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
};
use serde::Deserialize;

use crate::json::{AgentRpcError, ErrorResponse, SessionResponse};

/// Creates one Agent session.
pub struct NewSession;

impl JsonRpcSchema for NewSession {
    const ADDRESS: &'static str = "session.new";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("new", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("new", response);
    const MAX_REQUEST_BYTES: usize = 64;
    const MAX_RESPONSE_BYTES: usize = 32;
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Persistence {
    Persistent,
    Ephemeral,
}

impl From<Persistence> for SessionPersistence {
    fn from(value: Persistence) -> Self {
        match value {
            Persistence::Persistent => Self::Persistent,
            Persistence::Ephemeral => Self::Ephemeral,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewSessionRequest {
    persistence: Persistence,
}

/// Builds the JSON handler for [`NewSession`].
pub fn new_session_handler(runtime: Rc<AgentRuntime>) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let runtime = Rc::clone(&runtime);
        async move {
            let request = request.deserialize::<NewSessionRequest>()?;
            match runtime.new_session(request.persistence.into()).await {
                Ok(session) => response.write(&SessionResponse { session }).await,
                Err(RuntimeError::SessionCreate(SessionCreateError::WorkerStopped)) => {
                    response
                        .write(&ErrorResponse(AgentRpcError::WorkerStopped))
                        .await
                }
                Err(_error) => {
                    response
                        .write(&ErrorResponse(AgentRpcError::Persistence))
                        .await
                }
            }
        }
    }
}

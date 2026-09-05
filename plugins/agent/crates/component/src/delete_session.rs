use alloc::rc::Rc;

use barracuda_agent_runtime::{AgentRuntime, SessionDeleteError};
use barracuda_event_router::{
    json_schema, JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
};
use serde::Deserialize;

use crate::json::{parse_session, AgentRpcError, ErrorResponse};

/// Deletes one Agent session and its persistent state.
pub struct DeleteSession;

impl JsonRpcSchema for DeleteSession {
    const ADDRESS: &'static str = "session.delete";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("delete", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("delete", response);
    const MAX_REQUEST_BYTES: usize = 48;
    const MAX_RESPONSE_BYTES: usize = 31;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteSessionRequest<'a> {
    #[serde(borrow)]
    session: &'a str,
}

/// Builds the JSON handler for [`DeleteSession`].
pub fn delete_session_handler(runtime: Rc<AgentRuntime>) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let runtime = Rc::clone(&runtime);
        async move {
            let request = request.deserialize::<DeleteSessionRequest<'_>>()?;
            let session = match parse_session(request.session) {
                Ok(session) => session,
                Err(error) => return response.write(&ErrorResponse(error)).await,
            };
            let result = runtime.delete_session(session).await;
            match result {
                Ok(()) => response.write("{}").await,
                Err(SessionDeleteError::SessionNotFound(_)) => {
                    response
                        .write(&ErrorResponse(AgentRpcError::SessionNotFound))
                        .await
                }
                Err(SessionDeleteError::AlreadyDeleting(_)) => {
                    response
                        .write(&ErrorResponse(AgentRpcError::AlreadyDeleting))
                        .await
                }
                Err(SessionDeleteError::WorkerStopped) => {
                    response
                        .write(&ErrorResponse(AgentRpcError::WorkerStopped))
                        .await
                }
                Err(_error) => response.write(&ErrorResponse(AgentRpcError::Storage)).await,
            }
        }
    }
}

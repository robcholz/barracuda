use alloc::rc::Rc;

use barracuda_agent_runtime::AgentRuntime;
use barracuda_event_router::{
    json_schema, JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
};
use serde::Deserialize;

use crate::json::{AgentRpcError, ErrorResponse, SessionPage};

const MAX_PAGE_SIZE: usize = 16;

/// Lists a bounded page of live Agent sessions.
pub struct ListSessions;

impl JsonRpcSchema for ListSessions {
    const ADDRESS: &'static str = "session.list";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("list", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("list", response);
    const MAX_REQUEST_BYTES: usize = 64;
    const MAX_RESPONSE_BYTES: usize = 512;
}

const fn default_limit() -> usize {
    MAX_PAGE_SIZE
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListSessionsRequest {
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}

/// Builds the JSON handler for [`ListSessions`].
pub fn list_sessions_handler(runtime: Rc<AgentRuntime>) -> impl JsonHandler {
    move |_context, request: JsonRef, response: JsonWriter| {
        let runtime = Rc::clone(&runtime);
        async move {
            let request = request.deserialize::<ListSessionsRequest>()?;
            if request.limit == 0 || request.limit > MAX_PAGE_SIZE {
                return response
                    .write(&ErrorResponse(AgentRpcError::InvalidRequest))
                    .await;
            }

            // The runtime owns this snapshot; JSON serialization borrows its bounded page.
            let sessions = runtime.list_sessions().await;
            let end = request
                .offset
                .checked_add(request.limit)
                .map(|end| core::cmp::min(end, sessions.len()))
                .ok_or(barracuda_event_router::RpcError::InvalidFrameState)?;
            let page = sessions.get(request.offset..end).unwrap_or(&[]);
            let next_offset = (end < sessions.len()).then_some(end);
            response
                .write(&SessionPage {
                    sessions: page,
                    next_offset,
                })
                .await
        }
    }
}

use barracuda_event_router::{rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, Unary};

use crate::convert;

use super::{SessionRegistry, SessionRpcError};

pub use crate::dto::{ReasoningEffortDto, SetReasoningEffortRequest};

/// RPC corresponding to `SessionControl::set_reasoning_effort`.
pub struct SetReasoningEffort;

#[rpc_dynamic]
impl RpcMethod for SetReasoningEffort {
    const ADDRESS: &'static str = "session.set_reasoning_effort";
    type Request = SetReasoningEffortRequest;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`SetReasoningEffort`].
pub fn set_reasoning_effort_handler(
    registry: SessionRegistry,
) -> impl RpcHandler<SetReasoningEffort> {
    move |_context, request: RpcFrame<SetReasoningEffortRequest>| {
        let registry = registry.clone();
        async move {
            let request = *request.view()?;
            let session = convert::session_from_wire(request.session);
            let effort = convert::reasoning_effort_from_wire(request.effort);
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            Ok(control
                .set_reasoning_effort(effort)
                .await
                .map_err(convert::session_error_from_control))
        }
    }
}

use barracuda_event_router::{rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, Unary};

use crate::convert;

use super::{SessionRegistry, SessionRpcError};

pub use crate::dto::CancelRequest;

/// RPC corresponding to `SessionControl::cancel`.
pub struct Cancel;

#[rpc_dynamic]
impl RpcMethod for Cancel {
    const ADDRESS: &'static str = "session.cancel";
    type Request = CancelRequest;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`Cancel`].
pub fn cancel_handler(registry: SessionRegistry) -> impl RpcHandler<Cancel> {
    move |_context, request: RpcFrame<CancelRequest>| {
        let registry = registry.clone();
        async move {
            let session = convert::session_from_wire(request.view()?.session);
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            Ok(control
                .cancel()
                .await
                .map_err(convert::session_error_from_control))
        }
    }
}

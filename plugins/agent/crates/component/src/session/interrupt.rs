use barracuda_event_router::{rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, Unary};

use crate::convert;

use super::{SessionRegistry, SessionRpcError};

pub use crate::dto::InterruptRequest;

/// RPC corresponding to `SessionControl::interrupt`.
pub struct Interrupt;

#[rpc_dynamic]
impl RpcMethod for Interrupt {
    const ADDRESS: &'static str = "session.interrupt";
    type Request = InterruptRequest;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`Interrupt`].
pub fn interrupt_handler(registry: SessionRegistry) -> impl RpcHandler<Interrupt> {
    move |_context, request: RpcFrame<InterruptRequest>| {
        let registry = registry.clone();
        async move {
            let session = convert::session_from_wire(request.view()?.session);
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            Ok(control
                .interrupt()
                .await
                .map_err(convert::session_error_from_control))
        }
    }
}

use barracuda_event_router::{rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, Unary};

use crate::convert;

use super::{SessionRegistry, SessionRpcError};

pub use crate::dto::CloseRequest;

/// RPC corresponding to `SessionControl::close`.
pub struct Close;

#[rpc_dynamic]
impl RpcMethod for Close {
    const ADDRESS: &'static str = "session.close";
    type Request = CloseRequest;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`Close`].
pub fn close_handler(registry: SessionRegistry) -> impl RpcHandler<Close> {
    move |_context, request: RpcFrame<CloseRequest>| {
        let registry = registry.clone();
        async move {
            let session = convert::session_from_wire(request.view()?.session);
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            let result = control
                .close()
                .await
                .map_err(convert::session_error_from_control);
            if result.is_ok() {
                registry.remove(session);
            }
            Ok(result)
        }
    }
}

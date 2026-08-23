use barracuda_event_router::{rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, Unary};

use crate::convert;

use super::{SessionRegistry, SessionRpcError};

pub use crate::dto::{PermissionLevelDto, SetPermissionLevelRequest};

/// RPC corresponding to `SessionControl::set_permission_level`.
pub struct SetPermissionLevel;

#[rpc_dynamic]
impl RpcMethod for SetPermissionLevel {
    const ADDRESS: &'static str = "session.set_permission_level";
    type Request = SetPermissionLevelRequest;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`SetPermissionLevel`].
pub fn set_permission_level_handler(
    registry: SessionRegistry,
) -> impl RpcHandler<SetPermissionLevel> {
    move |_context, request: RpcFrame<SetPermissionLevelRequest>| {
        let registry = registry.clone();
        async move {
            let request = *request.view()?;
            let session = convert::session_from_wire(request.session);
            let level = convert::permission_level_from_wire(request.level);
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            Ok(control
                .set_permission_level(level)
                .await
                .map_err(convert::session_error_from_control))
        }
    }
}

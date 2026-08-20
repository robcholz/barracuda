use barracuda_agent_runtime::SessionId;
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, Unary};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::dto::SessionIdDto;

use super::{SessionRegistry, SessionRpcError};

/// Request corresponding to `SessionControl::close`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct CloseRequest {
    session: SessionIdDto,
}

impl CloseRequest {
    /// Creates a close request for `session`.
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        Self {
            session: session.into(),
        }
    }

    pub(crate) fn session(self) -> SessionId {
        self.session.into()
    }
}

/// RPC corresponding to `SessionControl::close`.
pub struct Close;

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
            let session = request.view()?.session();
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            let result = control.close().await.map_err(Into::into);
            if result.is_ok() {
                registry.remove(session);
            }
            Ok(result)
        }
    }
}

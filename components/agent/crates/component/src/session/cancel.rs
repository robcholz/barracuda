use barracuda_agent_runtime::SessionId;
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, Unary};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::dto::SessionIdDto;

use super::{SessionRegistry, SessionRpcError};

/// Request corresponding to `SessionControl::cancel`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct CancelRequest {
    session: SessionIdDto,
}

impl CancelRequest {
    /// Creates a cancel request for `session`.
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

/// RPC corresponding to `SessionControl::cancel`.
pub struct Cancel;

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
            let session = request.view()?.session();
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            Ok(control.cancel().await.map_err(Into::into))
        }
    }
}

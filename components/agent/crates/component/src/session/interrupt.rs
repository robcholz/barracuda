use barracuda_agent_runtime::SessionId;
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, Unary};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::dto::SessionIdDto;

use super::{SessionRegistry, SessionRpcError};

/// Request corresponding to `SessionControl::interrupt`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct InterruptRequest {
    session: SessionIdDto,
}

impl InterruptRequest {
    /// Creates an interrupt request for `session`.
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

/// RPC corresponding to `SessionControl::interrupt`.
pub struct Interrupt;

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
            let session = request.view()?.session();
            let Some(control) = registry.get(session) else {
                return Ok(Err(SessionRpcError::SessionNotOpen));
            };
            Ok(control.interrupt().await.map_err(Into::into))
        }
    }
}

use alloc::rc::Rc;

use barracuda_agent_runtime::{AgentRuntime, SessionDeleteError, SessionId};
use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, Unary};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::dto::SessionIdDto;

/// Request corresponding to `AgentRuntime::delete_session`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct DeleteSessionRequest {
    session: SessionIdDto,
}

impl From<SessionId> for DeleteSessionRequest {
    fn from(session: SessionId) -> Self {
        Self {
            session: session.into(),
        }
    }
}

impl DeleteSessionRequest {
    /// Creates a delete request for `session`.
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

/// Failure returned by `agent.delete_session`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum DeleteSessionError {
    /// The requested session does not exist.
    SessionNotFound,
    /// Deletion of this session is already in progress.
    AlreadyDeleting,
    /// The Agent runtime worker stopped.
    WorkerStopped,
    /// Persistent state could not be deleted.
    Storage,
}

/// RPC corresponding to `AgentRuntime::delete_session`.
pub struct DeleteSession;

impl RpcMethod for DeleteSession {
    const ADDRESS: &'static str = "agent.delete_session";
    type Request = DeleteSessionRequest;
    type Response = ();
    type Error = DeleteSessionError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`DeleteSession`].
pub fn delete_session_handler<Filesystem, Http>(
    runtime: Rc<AgentRuntime<Filesystem, Http>>,
) -> impl RpcHandler<DeleteSession>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    move |_context, request: RpcFrame<DeleteSessionRequest>| {
        let runtime = Rc::clone(&runtime);
        async move {
            let result = match runtime.delete_session(request.view()?.session()).await {
                Ok(()) => Ok(()),
                Err(SessionDeleteError::SessionNotFound(_)) => {
                    Err(DeleteSessionError::SessionNotFound)
                }
                Err(SessionDeleteError::AlreadyDeleting(_)) => {
                    Err(DeleteSessionError::AlreadyDeleting)
                }
                Err(SessionDeleteError::WorkerStopped) => Err(DeleteSessionError::WorkerStopped),
                Err(_error) => Err(DeleteSessionError::Storage),
            };
            Ok(result)
        }
    }
}

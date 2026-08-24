use alloc::rc::Rc;

use barracuda_agent_runtime::{AgentRuntime, SessionDeleteError};
use barracuda_event_router::{rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, Unary};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};

use crate::convert;

pub use crate::dto::{DeleteSessionError, DeleteSessionRequest};

/// RPC corresponding to `AgentRuntime::delete_session`.
pub struct DeleteSession;

#[rpc_dynamic]
impl RpcMethod for DeleteSession {
    const ADDRESS: &'static str = "session.delete";
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
            let session = convert::session_from_wire(request.view()?.session);
            let result = match runtime.delete_session(session).await {
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

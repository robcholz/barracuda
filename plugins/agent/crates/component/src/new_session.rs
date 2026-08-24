use alloc::rc::Rc;

use barracuda_agent_runtime::{AgentRuntime, RuntimeError, SessionCreateError};
use barracuda_event_router::{rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, Unary};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};

use crate::convert;

pub use crate::dto::{NewSessionError, NewSessionRequest, NewSessionResponse};

/// RPC corresponding to `AgentRuntime::new_session`.
pub struct NewSession;

#[rpc_dynamic]
impl RpcMethod for NewSession {
    const ADDRESS: &'static str = "session.new";
    type Request = NewSessionRequest;
    type Response = NewSessionResponse;
    type Error = NewSessionError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`NewSession`].
pub fn new_session_handler<Filesystem, Http>(
    runtime: Rc<AgentRuntime<Filesystem, Http>>,
) -> impl RpcHandler<NewSession>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    move |_context, request: RpcFrame<NewSessionRequest>| {
        let runtime = Rc::clone(&runtime);
        async move {
            let persistence = convert::persistence_from_wire(request.view()?.persistence);
            let response = match runtime.new_session(persistence).await {
                Ok(session) => Ok(NewSessionResponse {
                    session: convert::session_to_wire(session),
                }),
                Err(RuntimeError::SessionCreate(SessionCreateError::WorkerStopped)) => {
                    Err(NewSessionError::WorkerStopped)
                }
                Err(_error) => Err(NewSessionError::Persistence),
            };
            Ok(response)
        }
    }
}

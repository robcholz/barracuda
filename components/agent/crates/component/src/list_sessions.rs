use alloc::rc::Rc;

use barracuda_agent_runtime::{AgentRuntime, SessionId};
use barracuda_event_router::{RpcHandler, RpcMethod, RpcResult, RpcStream, Streaming, Unary};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};
use futures_lite::stream;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::dto::SessionIdDto;

/// One response item from `AgentRuntime::list_sessions`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub struct ListSessionsResponse {
    session: SessionIdDto,
}

impl From<SessionId> for ListSessionsResponse {
    fn from(session: SessionId) -> Self {
        Self {
            session: session.into(),
        }
    }
}

impl ListSessionsResponse {
    /// Returns the Agent domain Session id.
    #[must_use]
    pub fn session(self) -> SessionId {
        self.session.into()
    }
}

/// RPC corresponding to `AgentRuntime::list_sessions`.
pub struct ListSessions;

impl RpcMethod for ListSessions {
    const ADDRESS: &'static str = "agent.list_sessions";
    type Request = ();
    type Response = ListSessionsResponse;
    type Error = ();
    type Input = Unary;
    type Output = Streaming;
}

/// Builds the reusable handler for [`ListSessions`].
pub fn list_sessions_handler<Filesystem, Http>(
    runtime: Rc<AgentRuntime<Filesystem, Http>>,
) -> impl RpcHandler<ListSessions>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    move |_context, _request| {
        let runtime = Rc::clone(&runtime);
        async move {
            let responses = runtime
                .list_sessions()
                .await
                .into_iter()
                .map(|session| RpcResult::Ok(Ok(ListSessionsResponse::from(session))));
            Ok(RpcStream::new(stream::iter(responses)))
        }
    }
}

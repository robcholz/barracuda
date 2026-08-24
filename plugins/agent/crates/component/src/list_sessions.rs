use alloc::rc::Rc;
use alloc::vec::Vec;

use barracuda_agent_runtime::AgentRuntime;
use barracuda_event_router::{rpc_dynamic, RpcHandler, RpcMethod, RpcStream, Streaming, Unary};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};
use futures_lite::stream;

use crate::convert;
use crate::dto::{SessionIdDto, MAX_SESSIONS_PER_LIST_ITEM};

pub use crate::dto::ListSessionsResponse;

/// RPC corresponding to `AgentRuntime::list_sessions`.
pub struct ListSessions;

#[rpc_dynamic]
impl RpcMethod for ListSessions {
    const ADDRESS: &'static str = "session.list";
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
            let sessions = runtime.list_sessions().await;
            let mut items = Vec::new();
            for chunk in sessions.chunks(MAX_SESSIONS_PER_LIST_ITEM) {
                let mut item = [SessionIdDto::new(0); MAX_SESSIONS_PER_LIST_ITEM];
                for (slot, session) in item.iter_mut().zip(chunk) {
                    *slot = convert::session_to_wire(*session);
                }
                items.push(Ok(Ok(ListSessionsResponse {
                    count: u32::try_from(chunk.len()).unwrap_or(u32::MAX),
                    sessions: item,
                })));
            }
            if items.is_empty() {
                items.push(Ok(Ok(ListSessionsResponse {
                    count: 0,
                    sessions: [SessionIdDto::new(0); MAX_SESSIONS_PER_LIST_ITEM],
                })));
            }
            Ok(RpcStream::new(stream::iter(items)))
        }
    }
}

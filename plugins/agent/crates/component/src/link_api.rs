use alloc::rc::Rc;

use barracuda_agent_runtime::{AgentRuntime, ModelApiConfig};
use barracuda_event_router::{rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, Unary};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};

use crate::convert;

pub use crate::dto::{LinkApiError, LinkApiRequest};

/// RPC corresponding to `AgentRuntime::link_api`.
pub struct LinkApi;

#[rpc_dynamic]
impl RpcMethod for LinkApi {
    const ADDRESS: &'static str = "agent.link_api";
    type Request = LinkApiRequest;
    type Response = ();
    type Error = LinkApiError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`LinkApi`].
pub fn link_api_handler<Filesystem, Http>(
    runtime: Rc<AgentRuntime<Filesystem, Http>>,
) -> impl RpcHandler<LinkApi>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    move |_context, request: RpcFrame<LinkApiRequest>| {
        let runtime = Rc::clone(&runtime);
        async move {
            let request = *request.view()?;
            let api = link_api_config(&request);
            let purpose = convert::purpose_from_wire(request.purpose);
            match runtime.link_api(api, purpose, request.default) {
                Ok(()) => Ok(Ok(())),
                Err(_error) => Ok(Err(LinkApiError::InvalidConfiguration)),
            }
        }
    }
}

fn link_api_config(request: &LinkApiRequest) -> ModelApiConfig {
    let mut api = ModelApiConfig::new(
        convert::backend_from_wire(request.backend),
        request.api_key.as_str(),
        request.model.as_str(),
        request.base_url.as_str(),
    );
    api.timeout_ms = request.timeout_ms;
    api.max_tokens = request.max_tokens;
    api.image_max_bytes = request.image_max_bytes as usize;
    api
}

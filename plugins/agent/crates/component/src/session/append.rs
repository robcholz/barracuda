use barracuda_agent_runtime::Message;
use barracuda_event_router::{
    rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary,
};

use crate::convert;

use super::{SessionRegistry, SessionRpcError};

pub use crate::dto::AppendRequestFrame;

/// RPC corresponding to `SessionControl::append`.
pub struct Append;

#[rpc_dynamic]
impl RpcMethod for Append {
    const ADDRESS: &'static str = "session.append";
    type Request = AppendRequestFrame;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Streaming;
    type Output = Unary;
}

/// Builds the reusable handler for [`Append`].
pub fn append_handler(registry: SessionRegistry) -> impl RpcHandler<Append> {
    move |_context, mut frames: RpcStream<RpcFrame<AppendRequestFrame>>| {
        let registry = registry.clone();
        async move {
            while let Some(frame) = frames.next().await {
                let frame = *frame?.view()?;
                let session = convert::session_from_wire(frame.session);
                let message = Message::text(frame.text.as_str());
                let Some(control) = registry.get(session) else {
                    return Ok(Err(SessionRpcError::SessionNotOpen));
                };
                let outcome = control
                    .append(message)
                    .await
                    .map_err(convert::session_error_from_control);
                if let Err(error) = outcome {
                    return Ok(Err(error));
                }
            }
            Ok(Ok(()))
        }
    }
}

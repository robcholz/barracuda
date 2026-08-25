use barracuda_agent_runtime::Message;
use barracuda_event_router::{
    rpc_dynamic, RpcFrame, RpcHandler, RpcMethod, RpcStream, Streaming, Unary,
};

use crate::convert;

use super::{SessionRegistry, SessionRpcError};

pub use crate::dto::RespondRequestFrame;

/// RPC corresponding to `SessionControl::respond`.
pub struct Respond;

#[rpc_dynamic]
impl RpcMethod for Respond {
    const ADDRESS: &'static str = "session.respond";
    type Request = RespondRequestFrame;
    type Response = ();
    type Error = SessionRpcError;
    type Input = Streaming;
    type Output = Unary;
}

/// Builds the reusable handler for [`Respond`].
pub fn respond_handler(registry: SessionRegistry) -> impl RpcHandler<Respond> {
    move |_context, mut frames: RpcStream<RpcFrame<RespondRequestFrame>>| {
        let registry = registry.clone();
        async move {
            while let Some(frame) = frames.next().await {
                let frame = *frame?.view()?;
                let session = convert::session_from_wire(frame.session);
                let input_request = convert::input_request_from_wire(frame.request);
                let message = Message::text(frame.text.as_str());
                let Some(control) = registry.get(session) else {
                    return Ok(Err(SessionRpcError::SessionNotOpen));
                };
                let outcome = control
                    .respond(input_request, message)
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

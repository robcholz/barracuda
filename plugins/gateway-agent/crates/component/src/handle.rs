use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_agent_component::dto::FixedStr;
use barracuda_agent_component::session::append::{Append, AppendRequestFrame};
use barracuda_event_router::{
    Event, RpcContext, RpcError, RpcFrame, RpcHandler, RpcMethod, RpcResult, RpcStream, Streaming,
    Unary,
};
use barracuda_message_gateway_component::gateway_message_received::{
    gateway_event_from_frames, GatewayEventFrame, GatewayMessageReceived,
};
use futures_lite::future::yield_now;
use futures_lite::stream;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::component::BridgeState;

/// Business failure returned by `bridge.handle`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
pub enum BridgeHandleError {
    /// The inbound gateway frames did not decode to a message.
    InvalidMessage,
    /// The bound Agent session rejected the appended message.
    AppendFailed,
}

/// Inbound RPC: append one gateway message to the bound Agent session.
///
/// Registered as the ingress step of the gateway Workflow, so its request type
/// is the `gateway.message.received` Event message. The Agent turn's events are
/// delivered back to the gateway by the outbound pump, not by this RPC.
pub struct BridgeHandle;

impl RpcMethod for BridgeHandle {
    const ADDRESS: &'static str = "bridge.handle";
    type Request = <GatewayMessageReceived as Event>::Message;
    type Response = ();
    type Error = BridgeHandleError;
    type Input = Streaming;
    type Output = Unary;
}

/// Builds the reusable handler for [`BridgeHandle`].
pub(crate) fn bridge_handle_handler(state: Rc<BridgeState>) -> impl RpcHandler<BridgeHandle> {
    move |context: RpcContext, frames: RpcStream<RpcFrame<GatewayEventFrame>>| {
        let state = Rc::clone(&state);
        async move {
            // The outbound pump owns the session lease; wait until it exists so
            // `session.append` is accepted.
            while !state.opened.get() {
                yield_now().await;
            }
            let inbound = match gateway_event_from_frames(collect(frames).await?) {
                Ok(inbound) => inbound,
                Err(_error) => return Ok(Err(BridgeHandleError::InvalidMessage)),
            };
            *state.latest_reply_to.borrow_mut() = Some(inbound.message_id);
            let Some(session) = state.session.get() else {
                return Ok(Err(BridgeHandleError::AppendFailed));
            };
            let Some(text) = fit_message_text(&inbound.text) else {
                return Ok(Err(BridgeHandleError::InvalidMessage));
            };
            let request = AppendRequestFrame { session, text };
            let input = RpcStream::new(stream::iter([RpcResult::Ok(request)]));
            match context.client().call::<Append>(input)?.await? {
                Ok(_response) => Ok(Ok(())),
                Err(_error) => Ok(Err(BridgeHandleError::AppendFailed)),
            }
        }
    }
}

async fn collect(
    mut frames: RpcStream<RpcFrame<GatewayEventFrame>>,
) -> Result<Vec<GatewayEventFrame>, RpcError> {
    let mut collected = Vec::new();
    while let Some(frame) = frames.next().await {
        collected.push(*frame?.view()?);
    }
    Ok(collected)
}

/// Truncates message text to the Agent append frame's fixed capacity on a UTF-8
/// character boundary. Returns `None` only if the text cannot be represented
/// (an embedded NUL byte).
fn fit_message_text<const N: usize>(text: &str) -> Option<FixedStr<N>> {
    let capacity = FixedStr::<N>::capacity();
    let mut fitted = String::new();
    for character in text.chars() {
        let mut buffer = [0_u8; 4];
        let encoded = character.encode_utf8(&mut buffer);
        if fitted.len().saturating_add(encoded.len()) > capacity {
            break;
        }
        fitted.push(character);
    }
    FixedStr::new(&fitted).ok()
}

use alloc::{rc::Rc, string::String};

use barracuda_event_router::{rpc_dynamic, rpc_message, RpcFrame, RpcHandler, RpcMethod, Unary};
use gateway::{MessageGateway, MessageTarget, SendMessageRequest};
use serde::{Deserialize, Serialize};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use crate::route::GatewayRoute;
use crate::wire::{GatewaySendReceipt, GatewayText, GatewayWireError};

const CHANNEL_CAPACITY: usize = 32;
const CONVERSATION_CAPACITY: usize = 96;
const THREAD_CAPACITY: usize = 48;
const REPLY_TO_CAPACITY: usize = 96;
const TEXT_CAPACITY: usize = 238;

/// One bounded complete text message accepted by `gateway.send`.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewaySendRequest {
    channel: GatewayText<CHANNEL_CAPACITY>,
    conversation: GatewayText<CONVERSATION_CAPACITY>,
    thread: GatewayText<THREAD_CAPACITY>,
    reply_to: GatewayText<REPLY_TO_CAPACITY>,
    text: GatewayText<TEXT_CAPACITY>,
}

impl GatewaySendRequest {
    /// Builds a complete-message request without a reply target.
    ///
    /// # Errors
    ///
    /// Returns a wire error when any field exceeds its fixed RPC capacity.
    pub fn new(route: &GatewayRoute, text: &str) -> Result<Self, GatewayWireError> {
        Self::with_reply_to(route, text, None)
    }

    /// Builds a complete-message request with an optional provider reply target.
    ///
    /// # Errors
    ///
    /// Returns a wire error when any field exceeds its fixed RPC capacity.
    pub fn with_reply_to(
        route: &GatewayRoute,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<Self, GatewayWireError> {
        Ok(Self {
            channel: GatewayText::new(&route.channel)?,
            conversation: GatewayText::new(&route.conversation_id)?,
            thread: GatewayText::new(route.thread_id.as_deref().unwrap_or_default())?,
            reply_to: GatewayText::new(reply_to.unwrap_or_default())?,
            text: GatewayText::new(text)?,
        })
    }

    fn route(&self) -> Result<GatewayRoute, GatewayWireError> {
        let mut route = GatewayRoute::new(self.channel.as_str()?, self.conversation.as_str()?);
        let thread = self.thread.as_str()?;
        if !thread.is_empty() {
            route.thread_id = Some(thread.into());
        }
        Ok(route)
    }

    fn reply_to(&self) -> Result<Option<&str>, GatewayWireError> {
        let reply_to = self.reply_to.as_str()?;
        Ok((!reply_to.is_empty()).then_some(reply_to))
    }

    fn text(&self) -> Result<&str, GatewayWireError> {
        self.text.as_str()
    }
}

/// Business failure returned by `gateway.send`.
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
#[serde(rename_all = "snake_case")]
pub enum GatewaySendError {
    /// A request frame was not canonical.
    InvalidRequest,
    /// No provider is registered for the requested channel.
    UnknownChannel,
    /// The selected provider rejected or failed the delivery.
    Delivery,
    /// The provider receipt could not fit in the Gateway response contract.
    InvalidReceipt,
}

/// Sends one bounded complete text message.
pub struct GatewaySend;

#[rpc_dynamic]
impl RpcMethod for GatewaySend {
    const ADDRESS: &'static str = "gateway.send";
    type Request = GatewaySendRequest;
    type Response = GatewaySendReceipt;
    type Error = GatewaySendError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`GatewaySend`].
pub fn gateway_send_handler(gateway: Rc<MessageGateway>) -> impl RpcHandler<GatewaySend> {
    move |_context, frame: RpcFrame<GatewaySendRequest>| {
        let gateway = Rc::clone(&gateway);
        async move {
            let request = frame.view()?;
            let route = match request.route() {
                Ok(route) => route,
                Err(_error) => return Ok(Err(GatewaySendError::InvalidRequest)),
            };
            let text = match request.text() {
                Ok(text) => String::from(text),
                Err(_error) => return Ok(Err(GatewaySendError::InvalidRequest)),
            };
            let reply_to = match request.reply_to() {
                Ok(reply_to) => reply_to.map(Into::into),
                Err(_error) => return Ok(Err(GatewaySendError::InvalidRequest)),
            };
            let mut target = MessageTarget::new(route.channel, route.conversation_id);
            target.thread_id = route.thread_id;
            let mut outbound = SendMessageRequest::text(target, text);
            outbound.reply_to = reply_to;
            let receipt = match gateway.send_message(outbound).await {
                Ok(receipt) => receipt,
                Err(gateway::GatewayError::UnknownChannel { .. }) => {
                    return Ok(Err(GatewaySendError::UnknownChannel));
                }
                Err(_error) => return Ok(Err(GatewaySendError::Delivery)),
            };
            match GatewaySendReceipt::new(&receipt.message_id) {
                Ok(receipt) => Ok(Ok(receipt)),
                Err(_error) => Ok(Err(GatewaySendError::InvalidReceipt)),
            }
        }
    }
}

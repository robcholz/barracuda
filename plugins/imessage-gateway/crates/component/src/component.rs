use alloc::boxed::Box;
use alloc::rc::Rc;
use core::future::pending;

use async_channel::{Receiver, Sender};
use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventEmitter, RegisterContext,
    RpcResult, RpcStream, RunContext, UnregisterContext,
};
use gateway::MessageGateway;

use crate::gateway_message_received::{
    frames_from_gateway_event, GatewayInboundMessage, GatewayMessageReceived,
};
use crate::gateway_send::{gateway_send_handler, GatewaySend};
use crate::gateway_send_media::{gateway_send_media_handler, GatewaySendMedia};

/// Failure queueing a normalized message for Event emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GatewayIngressError {
    /// The matching Gateway Component has stopped.
    #[error("Gateway Component is not running")]
    Stopped,
}

/// Cloneable producer handle for transport-specific inbound adapters.
#[derive(Clone)]
pub struct GatewayIngress {
    messages: Sender<GatewayInboundMessage>,
}

impl GatewayIngress {
    /// Queues one normalized inbound message, awaiting bounded backpressure.
    pub async fn publish(&self, message: GatewayInboundMessage) -> Result<(), GatewayIngressError> {
        self.messages
            .send(message)
            .await
            .map_err(|_error| GatewayIngressError::Stopped)
    }
}

/// Event Router adapter for inbound Events and outbound Gateway RPCs.
pub struct GatewayComponent {
    gateway: Rc<MessageGateway>,
    messages: Receiver<GatewayInboundMessage>,
}

impl GatewayComponent {
    /// Creates a Component and its bounded inbound producer handle.
    #[must_use]
    pub fn new(gateway: MessageGateway, ingress_capacity: usize) -> (Self, GatewayIngress) {
        let (sender, messages) = async_channel::bounded(ingress_capacity.max(1));
        (
            Self {
                gateway: Rc::new(gateway),
                messages,
            },
            GatewayIngress { messages: sender },
        )
    }
}

impl<const M: usize> Component<M> for GatewayComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<GatewaySend, _>(gateway_send_handler(Rc::clone(&self.gateway)))?;
        context.register_rpc::<GatewaySendMedia, _>(gateway_send_media_handler(Rc::clone(
            &self.gateway,
        )))
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let emitter = EventEmitter::<M>::new(context.rpc().clone());
            loop {
                let message = match self.messages.recv().await {
                    Ok(message) => message,
                    Err(_closed) => return pending().await,
                };
                let chunks =
                    frames_from_gateway_event(&message).map_err(ComponentError::lifecycle)?;
                let stream = RpcStream::new(futures_lite::stream::iter(
                    chunks.into_iter().map(RpcResult::Ok),
                ));
                emitter
                    .emit::<GatewayMessageReceived>(stream)
                    .await
                    .map_err(ComponentError::lifecycle)?;
            }
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.messages.close();
        Ok(())
    }
}

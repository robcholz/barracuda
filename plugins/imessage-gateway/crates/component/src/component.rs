use alloc::{boxed::Box, rc::Rc};
use core::future::pending;

use async_channel::{Receiver, Sender};
use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventEmitter, RegisterContext,
    RunContext, UnregisterContext,
};
use gateway::MessageGateway;

use crate::gateway_message_received::{
    emit_inbound, validate_inbound, GatewayInboundMessage, GatewayMessageReceived,
    InboundValidationError,
};
use crate::gateway_send::{gateway_send_handler, GatewaySend};
use crate::gateway_send_media::{
    deliver_media_stream, gateway_send_media_handler, GatewaySendMedia, MediaJob, MediaSessions,
};
use crate::gateway_send_stream::{
    deliver_text_stream, gateway_send_stream_handler, GatewaySendStream, TextJob, TextSessions,
};
use crate::json::event_input_capacity;

pub(crate) const STREAM_WORKERS: usize = 4;

/// Failure queueing a normalized message for Event emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GatewayIngressError {
    /// Required route metadata or the provider message identifier is absent.
    #[error("inbound Gateway message is missing required metadata")]
    InvalidMessage,
    /// The complete encoded message cannot fit one Event document.
    #[error("inbound Gateway message exceeds the Event lane capacity")]
    MessageTooLarge,
    /// The matching Gateway ingress Component has stopped.
    #[error("Gateway ingress Component is not running")]
    Stopped,
}

/// Cloneable producer handle for transport-specific inbound adapters.
#[derive(Clone)]
pub struct GatewayIngress {
    messages: Sender<GatewayInboundMessage>,
    event_input_bytes: usize,
}

impl GatewayIngress {
    /// Queues one normalized inbound message, awaiting bounded backpressure.
    ///
    /// # Errors
    ///
    /// Returns [`GatewayIngressError::InvalidMessage`] when required metadata
    /// is absent, [`GatewayIngressError::MessageTooLarge`] when the complete
    /// Event does not fit one lane, or [`GatewayIngressError::Stopped`] after
    /// the ingress Component stops accepting messages.
    pub async fn publish(&self, message: GatewayInboundMessage) -> Result<(), GatewayIngressError> {
        match validate_inbound(&message, self.event_input_bytes) {
            Ok(()) => {}
            Err(InboundValidationError::InvalidMessage) => {
                return Err(GatewayIngressError::InvalidMessage);
            }
            Err(InboundValidationError::MessageTooLarge) => {
                return Err(GatewayIngressError::MessageTooLarge);
            }
        }
        self.messages
            .send(message)
            .await
            .map_err(|_error| GatewayIngressError::Stopped)
    }
}

/// Event Router adapter that owns all public Gateway JSON RPC registrations.
pub struct GatewayComponent {
    gateway: Rc<MessageGateway>,
    text_sessions: Rc<TextSessions>,
    media_sessions: Rc<MediaSessions>,
    text_jobs: Sender<TextJob>,
    media_jobs: Sender<MediaJob>,
}

/// Runtime Components that advance inbound and application-stream contracts.
pub struct GatewayRuntimeComponents {
    inbound: GatewayInboundComponent,
    text: [GatewayTextStreamComponent; STREAM_WORKERS],
    media: [GatewayMediaStreamComponent; STREAM_WORKERS],
}

impl GatewayRuntimeComponents {
    /// Separates the runtime graph into independently polled Components.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        GatewayInboundComponent,
        [GatewayTextStreamComponent; STREAM_WORKERS],
        [GatewayMediaStreamComponent; STREAM_WORKERS],
    ) {
        (self.inbound, self.text, self.media)
    }
}

impl GatewayComponent {
    /// Creates the RPC adapter, ingress handle, and bounded runtime workers.
    #[must_use]
    pub fn new<const M: usize>(
        gateway: MessageGateway,
        ingress_capacity: usize,
    ) -> (Self, GatewayIngress, GatewayRuntimeComponents) {
        let gateway = Rc::new(gateway);
        let (messages, inbound_messages) = async_channel::bounded(ingress_capacity.max(1));
        let (text_jobs, text_worker_jobs) = async_channel::bounded(STREAM_WORKERS);
        let (media_jobs, media_worker_jobs) = async_channel::bounded(STREAM_WORKERS);
        let text_sessions = Rc::new(TextSessions::default());
        let media_sessions = Rc::new(MediaSessions::default());
        let component = Self {
            gateway: Rc::clone(&gateway),
            text_sessions: Rc::clone(&text_sessions),
            media_sessions: Rc::clone(&media_sessions),
            text_jobs,
            media_jobs,
        };
        let runtime = GatewayRuntimeComponents {
            inbound: GatewayInboundComponent {
                messages: inbound_messages,
            },
            text: core::array::from_fn(|_| GatewayTextStreamComponent {
                gateway: Rc::clone(&gateway),
                sessions: Rc::clone(&text_sessions),
                jobs: text_worker_jobs.clone(),
            }),
            media: core::array::from_fn(|_| GatewayMediaStreamComponent {
                gateway: Rc::clone(&gateway),
                sessions: Rc::clone(&media_sessions),
                jobs: media_worker_jobs.clone(),
            }),
        };
        let event_input_bytes = event_input_capacity::<M>(
            <GatewayMessageReceived as barracuda_event_router::Event>::ID,
        )
        .unwrap_or_default();
        (
            component,
            GatewayIngress {
                messages,
                event_input_bytes,
            },
            runtime,
        )
    }
}

impl<const M: usize> Component<M> for GatewayComponent {
    fn name(&self) -> &'static str {
        "imessage-gateway"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context
            .register_json::<GatewaySend, _>("*", gateway_send_handler(Rc::clone(&self.gateway)))?;
        context.register_json::<GatewaySendStream, _>(
            "*",
            gateway_send_stream_handler(Rc::clone(&self.text_sessions), self.text_jobs.clone()),
        )?;
        context.register_json::<GatewaySendMedia, _>(
            "*",
            gateway_send_media_handler(Rc::clone(&self.media_sessions), self.media_jobs.clone()),
        )
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.text_jobs.close();
        self.media_jobs.close();
        self.text_sessions.clear();
        self.media_sessions.clear();
        Ok(())
    }
}

/// Component that emits bounded inbound Gateway JSON Events.
pub struct GatewayInboundComponent {
    messages: Receiver<GatewayInboundMessage>,
}

impl<const M: usize> Component<M> for GatewayInboundComponent {
    fn name(&self) -> &'static str {
        "imessage-gateway-inbound"
    }

    fn register(&mut self, _context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let emitter = EventEmitter::<M>::new(context.rpc().clone());
            loop {
                let message = match self.messages.recv().await {
                    Ok(message) => message,
                    Err(_closed) => return pending().await,
                };
                emit_inbound(&emitter, &message)
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

/// Component that drives accepted outbound text streams to channel providers.
pub struct GatewayTextStreamComponent {
    gateway: Rc<MessageGateway>,
    sessions: Rc<TextSessions>,
    jobs: Receiver<TextJob>,
}

impl<const M: usize> Component<M> for GatewayTextStreamComponent {
    fn name(&self) -> &'static str {
        "imessage-gateway-text-stream"
    }

    fn register(&mut self, _context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let emitter = EventEmitter::<M>::new(context.rpc().clone());
            loop {
                let job = match self.jobs.recv().await {
                    Ok(job) => job,
                    Err(_closed) => return pending().await,
                };
                deliver_text_stream(&self.gateway, &self.sessions, &emitter, job)
                    .await
                    .map_err(ComponentError::lifecycle)?;
            }
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.jobs.close();
        self.sessions.clear();
        Ok(())
    }
}

/// Component that drives accepted outbound media streams to channel providers.
pub struct GatewayMediaStreamComponent {
    gateway: Rc<MessageGateway>,
    sessions: Rc<MediaSessions>,
    jobs: Receiver<MediaJob>,
}

impl<const M: usize> Component<M> for GatewayMediaStreamComponent {
    fn name(&self) -> &'static str {
        "imessage-gateway-media-stream"
    }

    fn register(&mut self, _context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let emitter = EventEmitter::<M>::new(context.rpc().clone());
            loop {
                let job = match self.jobs.recv().await {
                    Ok(job) => job,
                    Err(_closed) => return pending().await,
                };
                deliver_media_stream(&self.gateway, &self.sessions, &emitter, job)
                    .await
                    .map_err(ComponentError::lifecycle)?;
            }
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.jobs.close();
        self.sessions.clear();
        Ok(())
    }
}

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
    deliver_event_stream, gateway_send_stream_handler, EventJob, EventSessions, GatewaySendStream,
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
        log::info!(
            "IMessage Gateway received inbound message `{}` on `{}` conversation `{}` ({} text bytes)",
            message.message_id,
            message.route.channel,
            message.route.conversation_id,
            message.text.len()
        );
        match validate_inbound(&message, self.event_input_bytes) {
            Ok(()) => {}
            Err(InboundValidationError::InvalidMessage) => {
                log::warn!(
                    "IMessage Gateway rejected inbound message `{}`: invalid metadata",
                    message.message_id
                );
                return Err(GatewayIngressError::InvalidMessage);
            }
            Err(InboundValidationError::MessageTooLarge) => {
                log::warn!(
                    "IMessage Gateway rejected inbound message `{}`: Event document exceeds {} bytes",
                    message.message_id,
                    self.event_input_bytes
                );
                return Err(GatewayIngressError::MessageTooLarge);
            }
        }
        let message_id = message.message_id.clone();
        match self.messages.send(message).await {
            Ok(()) => {
                log::debug!("IMessage Gateway queued inbound message `{message_id}`");
                Ok(())
            }
            Err(_error) => {
                log::warn!(
                    "IMessage Gateway could not queue inbound message `{message_id}`: ingress stopped"
                );
                Err(GatewayIngressError::Stopped)
            }
        }
    }
}

/// Event Router adapter that owns all public Gateway JSON RPC registrations.
pub struct GatewayComponent {
    gateway: Rc<MessageGateway>,
    event_sessions: Rc<EventSessions>,
    media_sessions: Rc<MediaSessions>,
    event_jobs: Sender<EventJob>,
    media_jobs: Sender<MediaJob>,
}

/// Runtime Components that advance inbound and application-stream contracts.
pub struct GatewayRuntimeComponents {
    inbound: GatewayInboundComponent,
    events: [GatewayEventStreamComponent; STREAM_WORKERS],
    media: [GatewayMediaStreamComponent; STREAM_WORKERS],
}

impl GatewayRuntimeComponents {
    /// Separates the runtime graph into independently polled Components.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        GatewayInboundComponent,
        [GatewayEventStreamComponent; STREAM_WORKERS],
        [GatewayMediaStreamComponent; STREAM_WORKERS],
    ) {
        (self.inbound, self.events, self.media)
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
        let (event_jobs, event_worker_jobs) = async_channel::bounded(STREAM_WORKERS);
        let (media_jobs, media_worker_jobs) = async_channel::bounded(STREAM_WORKERS);
        let event_sessions = Rc::new(EventSessions::default());
        let media_sessions = Rc::new(MediaSessions::default());
        let component = Self {
            gateway: Rc::clone(&gateway),
            event_sessions: Rc::clone(&event_sessions),
            media_sessions: Rc::clone(&media_sessions),
            event_jobs,
            media_jobs,
        };
        let runtime = GatewayRuntimeComponents {
            inbound: GatewayInboundComponent {
                messages: inbound_messages,
            },
            events: core::array::from_fn(|_| GatewayEventStreamComponent {
                gateway: Rc::clone(&gateway),
                sessions: Rc::clone(&event_sessions),
                jobs: event_worker_jobs.clone(),
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
            gateway_send_stream_handler(Rc::clone(&self.event_sessions), self.event_jobs.clone()),
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
        self.event_jobs.close();
        self.media_jobs.close();
        self.event_sessions.clear();
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
                log::info!(
                    "IMessage Gateway emitting `gateway.message.received` for message `{}`",
                    message.message_id
                );
                if let Err(error) = emit_inbound(&emitter, &message).await {
                    log::error!(
                        "IMessage Gateway failed to emit inbound message `{}`: {error}",
                        message.message_id
                    );
                    return Err(ComponentError::lifecycle(error));
                }
                log::debug!(
                    "IMessage Gateway Event Router accepted inbound message `{}`",
                    message.message_id
                );
            }
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.messages.close();
        Ok(())
    }
}

/// Component that drives accepted semantic event streams to channel providers.
pub struct GatewayEventStreamComponent {
    gateway: Rc<MessageGateway>,
    sessions: Rc<EventSessions>,
    jobs: Receiver<EventJob>,
}

impl<const M: usize> Component<M> for GatewayEventStreamComponent {
    fn name(&self) -> &'static str {
        "imessage-gateway-event-stream"
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
                deliver_event_stream(&self.gateway, &self.sessions, &emitter, job)
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

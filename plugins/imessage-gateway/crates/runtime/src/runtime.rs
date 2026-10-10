use alloc::rc::Rc;

use async_channel::{Receiver, Sender};
use barracuda_workflow_plugin::WorkflowService;
use gateway::MessageGateway;

use crate::gateway_control_received::{
    valid_control, GatewayControlReceived, GatewayInboundControl,
};
use crate::gateway_message_received::{
    valid_inbound, GatewayInboundMessage, GatewayMessageReceived,
};
use crate::gateway_send::{send, GatewaySendRequest, GatewaySendResponse};
use crate::gateway_send_media::{
    accept_media, deliver_media_stream, GatewaySendMediaRequest, MediaJob, MediaSessions,
};
use crate::gateway_send_stream::{
    accept_stream, deliver_event_stream, EventJob, EventSessions, GatewaySendStreamRequest,
};
use crate::json::{GatewayAccepted, GatewayOperationError};

pub(crate) const STREAM_WORKERS: usize = 4;

/// Failure publishing a normalized inbound Gateway message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GatewayIngressError {
    /// Required route metadata or the provider message identifier is absent.
    #[error("inbound Gateway message is missing required metadata")]
    InvalidMessage,
    /// The Workflow runtime rejected or could not encode the Event.
    #[error("inbound Gateway Event delivery failed")]
    Delivery,
}

/// Cloneable producer handle for transport-specific inbound adapters.
#[derive(Clone)]
pub struct GatewayIngress {
    workflow: Rc<WorkflowService>,
}

impl GatewayIngress {
    /// Waits until Workflow can take another `gateway.message.received`.
    ///
    /// Resolves once fewer than the Workflow Runtime's backlog limit of
    /// executions started by that Event are queued or running. Receive loops
    /// await it before fetching the next batch so a burst of inbound messages
    /// waits upstream instead of in RAM.
    pub async fn ready(&self) {
        self.workflow.ready_for::<GatewayMessageReceived>().await;
    }

    /// Publishes one normalized inbound message as a Workflow Event.
    pub async fn publish(&self, message: GatewayInboundMessage) -> Result<(), GatewayIngressError> {
        if !valid_inbound(&message) {
            return Err(GatewayIngressError::InvalidMessage);
        }
        let input =
            serde_json::to_value(message).map_err(|_error| GatewayIngressError::Delivery)?;
        self.workflow
            .emit::<GatewayMessageReceived>(input)
            .map_err(|_error| GatewayIngressError::Delivery)
    }

    /// Publishes one normalized control request as a Workflow Event.
    ///
    /// Controls skip [`Self::ready`]: one stands for a user's click on the
    /// running turn and must not wait behind queued messages.
    pub fn publish_control(
        &self,
        control: GatewayInboundControl,
    ) -> Result<(), GatewayIngressError> {
        if !valid_control(&control) {
            return Err(GatewayIngressError::InvalidMessage);
        }
        let input =
            serde_json::to_value(control).map_err(|_error| GatewayIngressError::Delivery)?;
        self.workflow
            .emit::<GatewayControlReceived>(input)
            .map_err(|_error| GatewayIngressError::Delivery)
    }
}

/// Shared Gateway operation runtime used by Agent tools and Workflow Actions.
pub struct GatewayRuntime {
    gateway: Rc<MessageGateway>,
    workflow: Rc<WorkflowService>,
    event_sessions: EventSessions,
    media_sessions: MediaSessions,
    event_jobs: Sender<EventJob>,
    event_worker_jobs: Receiver<EventJob>,
    media_jobs: Sender<MediaJob>,
    media_worker_jobs: Receiver<MediaJob>,
}

impl GatewayRuntime {
    /// Creates the typed operation runtime and inbound producer.
    #[must_use]
    pub fn new(
        gateway: Rc<MessageGateway>,
        workflow: Rc<WorkflowService>,
    ) -> (Rc<Self>, GatewayIngress) {
        let (event_jobs, event_worker_jobs) = async_channel::bounded(STREAM_WORKERS);
        let (media_jobs, media_worker_jobs) = async_channel::bounded(STREAM_WORKERS);
        let ingress = GatewayIngress {
            workflow: Rc::clone(&workflow),
        };
        (
            Rc::new(Self {
                gateway,
                workflow,
                event_sessions: EventSessions::default(),
                media_sessions: MediaSessions::default(),
                event_jobs,
                event_worker_jobs,
                media_jobs,
                media_worker_jobs,
            }),
            ingress,
        )
    }

    /// Sends one complete text message.
    pub async fn send(
        &self,
        request: GatewaySendRequest,
    ) -> Result<GatewaySendResponse, GatewayOperationError> {
        send(&self.gateway, request).await
    }

    /// Accepts one semantic event for an outbound text stream.
    pub fn send_stream(
        &self,
        request: GatewaySendStreamRequest,
    ) -> Result<GatewayAccepted, GatewayOperationError> {
        accept_stream(&self.event_sessions, &self.event_jobs, request)
    }

    /// Accepts one command for an outbound binary media stream.
    pub fn send_media(
        &self,
        request: GatewaySendMediaRequest,
    ) -> Result<GatewayAccepted, GatewayOperationError> {
        accept_media(
            &self.gateway,
            &self.media_sessions,
            &self.media_jobs,
            request,
        )
    }

    /// Runs one of the fixed outbound semantic-stream workers.
    pub async fn run_event_worker(&self) {
        while let Ok(job) = self.event_worker_jobs.recv().await {
            deliver_event_stream(&self.gateway, &self.event_sessions, &self.workflow, job).await;
        }
    }

    /// Runs one of the fixed outbound media-stream workers.
    pub async fn run_media_worker(&self) {
        while let Ok(job) = self.media_worker_jobs.recv().await {
            deliver_media_stream(&self.gateway, &self.media_sessions, &self.workflow, job).await;
        }
    }
}

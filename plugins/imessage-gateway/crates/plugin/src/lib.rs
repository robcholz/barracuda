//! Base IMessage Gateway Plugin and provider-registration capability.

#![no_std]

extern crate alloc;

mod workflow;

use alloc::rc::Rc;

use barracuda_imessage_gateway_runtime::{GatewayIngress, GatewayRuntime};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use barracuda_workflow_plugin::{WorkflowActionRegistry, WorkflowService};
use embassy_futures::select::select;

pub use barracuda_imessage_gateway_runtime::{
    GatewayAccepted, GatewayInboundMessage, GatewayIngressError, GatewayMediaKind,
    GatewayMessageReceived, GatewayOperationError, GatewayRoute, GatewaySendMediaFinished,
    GatewaySendMediaRequest, GatewaySendRequest, GatewaySendResponse, GatewaySendStreamFinished,
    GatewaySendStreamRequest,
};
pub use gateway::*;
pub use gateway_http::*;

const STREAM_WORKERS: usize = 4;

/// Typed capability shared by channel providers, Agent tools, and Workflow Actions.
pub struct IMessageGateway {
    gateway: Rc<MessageGateway>,
    ingress: GatewayIngress,
    runtime: Rc<GatewayRuntime>,
}

impl IMessageGateway {
    /// Registers one outbound message channel.
    pub fn register(
        &self,
        channel: Rc<dyn MessageChannel>,
    ) -> Result<MessageChannelRegistration, GatewayError> {
        self.gateway.register(channel)
    }

    /// Waits until Workflow can take another `gateway.message.received`.
    ///
    /// Backed by `WorkflowService::ready_for`: it resolves while fewer than
    /// the Workflow Runtime's backlog limit (`EVENT_BACKLOG_LIMIT`, 4) of
    /// executions started by that Event are queued or running, and resolves
    /// at once when no Workflow listens to it. Receive loops await it before
    /// fetching the next batch. It is cancellation-safe.
    pub async fn ready(&self) {
        self.ingress.ready().await;
    }

    /// Publishes one normalized inbound message into Workflow matching.
    pub async fn publish(&self, message: GatewayInboundMessage) -> Result<(), GatewayIngressError> {
        self.ingress.publish(message).await
    }

    /// Sends one complete text message.
    pub async fn send(
        &self,
        request: GatewaySendRequest,
    ) -> Result<GatewaySendResponse, GatewayOperationError> {
        self.runtime.send(request).await
    }

    /// Accepts one semantic event for an outbound text stream.
    pub fn send_stream(
        &self,
        request: GatewaySendStreamRequest,
    ) -> Result<GatewayAccepted, GatewayOperationError> {
        self.runtime.send_stream(request)
    }

    /// Accepts one command for an outbound binary media stream.
    pub fn send_media(
        &self,
        request: GatewaySendMediaRequest,
    ) -> Result<GatewayAccepted, GatewayOperationError> {
        self.runtime.send_media(request)
    }
}

/// Plugin that owns the shared IMessage Gateway capability and workers.
#[barracuda_plugin::macros::plugin]
pub struct IMessageGatewayPlugin {
    runtime: Option<Rc<GatewayRuntime>>,
}

impl IMessageGatewayPlugin {
    /// Creates the base IMessage Gateway Plugin.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { runtime: None }
    }
}

impl Plugin for IMessageGatewayPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let workflow_service = context.require::<WorkflowService>("workflow")?;
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let gateway = Rc::new(MessageGateway::new());
        let (runtime, ingress) =
            GatewayRuntime::new(Rc::clone(&gateway), Rc::clone(&workflow_service));
        let capability = Rc::new(IMessageGateway {
            gateway,
            ingress,
            runtime: Rc::clone(&runtime),
        });
        for registration in workflow::register_actions(&actions, Rc::clone(&capability))
            .map_err(PluginError::registration)?
        {
            context.retain(registration);
        }
        context.provide(capability)?;
        self.runtime = Some(runtime);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(GatewayRuntimeUnavailable))?;
        let spawner = context.task_spawner()?;
        for _worker in 0..STREAM_WORKERS {
            let event = gateway_event_worker(Rc::clone(&runtime), context.task_token())
                .map_err(PluginError::registration)?;
            spawner.spawn(event);
            let media = gateway_media_worker(Rc::clone(&runtime), context.task_token())
                .map_err(PluginError::registration)?;
            spawner.spawn(media);
        }
        Ok(())
    }
}

#[embassy_executor::task(pool_size = STREAM_WORKERS)]
async fn gateway_event_worker(runtime: Rc<GatewayRuntime>, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime.run_event_worker()).await;
}

#[embassy_executor::task(pool_size = STREAM_WORKERS)]
async fn gateway_media_worker(runtime: Rc<GatewayRuntime>, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime.run_media_worker()).await;
}

#[derive(Debug, thiserror::Error)]
#[error("Gateway runtime was not prepared during Plugin registration")]
struct GatewayRuntimeUnavailable;

//! The IMessage Gateway Plugin: message routing between channels and
//! Workflow, and everything channel Plugins share (mode, owners, receive
//! loops and slots, their HTTP routes and status, and HTTP helpers), all
//! reached through the [`IMessageGateway`] capability.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod channel;
mod http;
mod owners;
mod workflow;

mod config {
    barracuda_plugin::config!();
}

use alloc::rc::Rc;

use barracuda_imessage_gateway_runtime::{GatewayIngress, GatewayRuntime};
use barracuda_plugin::api::{PluginContext, SharedEntropy, Stack};
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use barracuda_plugin::manager::{PluginStorage, StorageError};
use barracuda_webserver_plugin::{HttpEndpoint, WebRouteRegistration, WebServer, WebServerError};
use barracuda_workflow_plugin::{WorkflowActionRegistry, WorkflowService};
use embassy_futures::select::select;
use http_client::{ClientFactory, ReceiveBuffers, ReceiveSlots};

pub use barracuda_imessage_gateway_runtime::{
    parse_command, GatewayAccepted, GatewayControlKind, GatewayControlReceived,
    GatewayInboundControl, GatewayInboundMessage, GatewayIngressError, GatewayMediaKind,
    GatewayMessageReceived, GatewayOperationError, GatewayRoute, GatewaySendMediaFinished,
    GatewaySendMediaRequest, GatewaySendRequest, GatewaySendResponse, GatewaySendStreamFinished,
    GatewaySendStreamRequest,
};
pub use channel::{
    ChannelControl, ChannelLevel, ChannelMode, ChannelStatus, ModeError, ModeFuture, NoSlot,
    OnDemand, ReceiveChannel, ReceiveControl, ReceiveError, ReceiveFuture, ReceiveRuntime,
    ReceiveSession, ReceiveSlotSource, ReceiveState, ReceiveTiming, SlotWait, UnlimitedSlots,
};
pub use gateway::*;
pub use http::{
    Error, GatewayHttp, Method, Multipart, MultipartBody, MultipartError, PollEnd, PollLimits,
    PollRequest, Polled, Poller, RequestBody, Response, SEND_TIMEOUT,
};
pub use http_client::ReceiveLease;
pub use owners::{Classification, Owner, Owners, OwnersError, PairingView, PAIRED_REPLY};

const STREAM_WORKERS: usize = 4;

/// Socket buffers of the channels' receive slots, as many as the selected
/// Board configures.
static RECEIVE_BUFFERS: ReceiveBuffers<{ config::RECEIVE_SLOTS }> = ReceiveBuffers::new();

/// Typed capability shared by channel providers, Agent tools, and Workflow Actions.
pub struct IMessageGateway {
    gateway: Rc<MessageGateway>,
    ingress: GatewayIngress,
    runtime: Rc<GatewayRuntime>,
    webserver: Rc<WebServer>,
    http: GatewayHttp,
    receive_slots: ReceiveSlots,
    entropy: owners::PairingEntropy,
}

/// Channel services: what every channel Plugin shares.
impl IMessageGateway {
    /// HTTP helpers over the http Plugin's clients.
    #[must_use]
    pub fn http(&self) -> GatewayHttp {
        self.http.clone()
    }

    /// Reads a channel's stored mode from its `storage`, storing `legacy`
    /// for a channel `configured` before modes existed and the default for
    /// a new one.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when reading or writing the mode fails.
    pub async fn load_mode<Storage: PluginStorage>(
        &self,
        storage: &Storage,
        configured: bool,
        legacy: ChannelMode,
    ) -> Result<ChannelMode, StorageError> {
        channel::load_mode(storage, configured, legacy).await
    }

    /// Stores a channel's `mode` in its `storage`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the write fails.
    pub async fn store_mode<Storage: PluginStorage>(
        &self,
        storage: &Storage,
        mode: ChannelMode,
    ) -> Result<(), StorageError> {
        channel::store_mode(storage, mode).await
    }

    /// Loads a channel's owner book from its `storage`; pairing codes come
    /// from the Platform's entropy.
    ///
    /// # Errors
    ///
    /// Returns [`OwnersError`] when the stored book cannot be read.
    pub async fn load_owners<Storage: PluginStorage>(
        &self,
        storage: Storage,
    ) -> Result<Owners<Storage>, OwnersError> {
        Owners::load(storage, self.entropy.clone()).await
    }

    /// A disabled receive control that leases one of the Gateway's receive
    /// slots while enabled.
    #[must_use]
    pub fn receive_control(&self) -> ReceiveControl<ReceiveSlots> {
        ReceiveControl::new(self.receive_slots.clone())
    }

    /// A disabled receive control for a channel that receives over the
    /// device's own HTTP server, such as a webhook, and holds no slot.
    #[must_use]
    pub fn webhook_receive_control(&self) -> ReceiveControl<UnlimitedSlots> {
        ReceiveControl::new(UnlimitedSlots)
    }

    /// Builds the receive runtime that must run as the channel Plugin's
    /// receive task, raced against its `PluginTaskToken`.
    #[must_use]
    pub fn receive_runtime<Slots, Channel>(
        &self,
        control: Rc<ReceiveControl<Slots>>,
        channel: Rc<Channel>,
        timing: ReceiveTiming,
    ) -> ReceiveRuntime
    where
        Slots: ReceiveSlotSource,
        Channel: ReceiveChannel<Slots::Lease>,
    {
        channel::receive_runtime(control, channel, timing)
    }

    /// Enables receiving exactly when `channel` is configured and in
    /// `send_receive`. Call it after loading the mode in `register` and after
    /// every configuration change.
    ///
    /// # Errors
    ///
    /// Returns [`NoSlot`] when receiving should run but every slot is in use.
    pub fn sync_receive<C: ChannelControl + ?Sized>(&self, channel: &C) -> Result<(), NoSlot> {
        channel::sync_receive(channel)
    }

    /// Serves a channel's HTTP routes under `base`: `config` on `base`
    /// itself, and the shared `GET base/status`, `base/mode`, and
    /// `base/owners`.
    ///
    /// # Errors
    ///
    /// Returns [`WebServerError`] when `base` cannot be registered.
    pub fn serve_channel<C, Config>(
        &self,
        base: &'static str,
        channel: Rc<C>,
        config: Config,
    ) -> Result<WebRouteRegistration, WebServerError>
    where
        C: ChannelControl,
        Config: HttpEndpoint,
    {
        self.webserver
            .serve_http_prefix(base, channel::ChannelEndpoint::new(channel, base, config))
    }

    /// The channel's status in a few words, for its portal card.
    #[must_use]
    pub fn channel_status<C: ChannelControl + ?Sized>(&self, channel: &C) -> ChannelStatus {
        channel::channel_status(channel)
    }
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

    /// Publishes one normalized control request (stop or rewind the running
    /// turn) into Workflow matching. It does not wait for message backlog.
    pub fn publish_control(
        &self,
        control: GatewayInboundControl,
    ) -> Result<(), GatewayIngressError> {
        self.ingress.publish_control(control)
    }

    /// Sends one complete text message.
    pub async fn send(
        &self,
        request: GatewaySendRequest,
    ) -> Result<GatewaySendResponse, GatewayOperationError> {
        self.runtime.send(request).await
    }

    /// Shows a conversation's sessions after a session command: as text by
    /// default, or however the conversation's channel overrides
    /// [`MessageChannel::send_sessions`].
    pub async fn send_sessions(&self, request: SendSessionsRequest) -> Result<(), GatewayError> {
        self.gateway.send_sessions(request).await
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

/// Plugin that owns the shared IMessage Gateway capability, the channels'
/// receive slots, and the Gateway workers.
#[barracuda_plugin::macros::plugin]
pub struct IMessageGatewayPlugin {
    ip_stack: Stack<'static>,
    entropy: SharedEntropy,
    runtime: Option<Rc<GatewayRuntime>>,
}

impl IMessageGatewayPlugin {
    /// Creates the base IMessage Gateway Plugin on the Platform IP stack,
    /// minting pairing codes from the Platform's entropy.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            ip_stack: context.ip_stack,
            entropy: context.entropy.clone(),
            runtime: None,
        }
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
        let http_clients = context.require::<ClientFactory<'static>>("http")?;
        let webserver = context.require::<WebServer>("webserver")?;
        let receive_slots = ReceiveSlots::new(self.ip_stack, &http_clients, RECEIVE_BUFFERS.take());
        log::info!("channel receive slots: {}", receive_slots.capacity());
        let gateway = Rc::new(MessageGateway::new());
        let (runtime, ingress) =
            GatewayRuntime::new(Rc::clone(&gateway), Rc::clone(&workflow_service));
        let capability = Rc::new(IMessageGateway {
            gateway,
            ingress,
            runtime: Rc::clone(&runtime),
            webserver,
            http: GatewayHttp::new((*http_clients).clone()),
            receive_slots,
            entropy: owners::PairingEntropy::new(self.entropy.clone()),
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

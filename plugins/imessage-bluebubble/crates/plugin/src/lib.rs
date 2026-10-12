//! IMessage BlueBubbles provider Plugin.
//!
//! Sends through the BlueBubbles REST API and, in `send_receive`, receives by
//! a webhook the BlueBubbles server posts to the device's own webserver.

#![no_std]
#![recursion_limit = "256"]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod channel;
mod receive;
mod state;
mod webhook;

#[cfg(test)]
mod tests;

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;

use barracuda_captive_portal_plugin::{
    CaptivePortal, EntryState, EntryStatus, ResourceFiles, WebEntry, WebGroup, WebText,
};
use barracuda_imessage_gateway_plugin::{
    ChannelControl, ChannelLevel, ChannelStatus, IMessageGateway, ReceiveRuntime, ReceiveTiming,
};
use barracuda_plugin::api::{PluginContext, SharedEntropy, Stack};
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginStorage,
    PluginTaskToken,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebRouteRegistration,
    WebServer, WebServerError,
};
use embassy_futures::select::select;

use crate::channel::{
    build_channel, BlueBubblesChannel, ConfigRequest, ConfigureError, LocalAddress,
};
use crate::state::HOOK_PATH;
use crate::webhook::WebhookEndpoint;

/// HTTP path accepting BlueBubbles configuration; the Gateway serves the
/// channel's `/status`, `/mode`, and `/owners` below it.
pub const CONFIG_API_PATH: &str = "/api/gateway/bluebubbles";
/// HTTP path prefix of the webhook; the secret follows it.
pub const WEBHOOK_API_PATH: &str = HOOK_PATH;

const JSON_CONTENT_TYPE: &str = "application/json";

/// Plugin that exposes BlueBubbles configuration, registers the resulting
/// channel, and receives its messages by webhook.
#[barracuda_plugin::macros::plugin]
pub struct IMessageBlueBubblePlugin {
    stack: Stack<'static>,
    entropy: SharedEntropy,
    runtime: Option<ReceiveRuntime>,
}

impl IMessageBlueBubblePlugin {
    /// Creates the unregistered BlueBubbles channel Plugin on the IP stack
    /// whose address the webhook names, minting webhook secrets from the
    /// Platform entropy.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            stack: context.ip_stack,
            entropy: context.entropy.clone(),
            runtime: None,
        }
    }
}

impl Plugin for IMessageBlueBubblePlugin {
    const REQUIREMENTS: barracuda_plugin::manager::PluginRequirements =
        barracuda_plugin::manager::PluginRequirements::new()
            .with_filesystem(barracuda_plugin::manager::PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let portal = context.require::<CaptivePortal>("captive-portal")?;
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let webserver = context.require::<WebServer>("webserver")?;
        let (channel, runtime) = build_channel(
            context.storage().clone(),
            gateway,
            self.entropy.clone(),
            lan_address(self.stack),
            ReceiveTiming::DEVICE,
        )?;
        let status = Rc::clone(&channel);
        context.retain(
            portal
                .register_with_status(
                    WebEntry {
                        id: "imessage-bluebubble",
                        group: WebGroup::Channel,
                        order: 50,
                        title: WebText {
                            zh: "BlueBubbles",
                            en: "BlueBubbles",
                        },
                        summary: WebText {
                            zh: "经 BlueBubbles 接入 iMessage",
                            en: "iMessage through BlueBubbles",
                        },
                        icon: Some("icon.svg"),
                        figure: None,
                        module: "entry.js",
                    },
                    ResourceFiles::from(context.filesystem()?.clone()),
                    move || entry_status(status.gateway.channel_status(&*status)),
                )
                .map_err(PluginError::registration)?,
        );
        for route in serve_routes(&webserver, channel).map_err(PluginError::registration)? {
            context.retain(route);
        }
        self.runtime = Some(runtime);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(ReceiveRuntimeUnavailable))?;
        let task = bluebubbles_receive_task(runtime, context.task_token())
            .map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

/// The station IPv4 address of `stack`, which the webhook URL names.
fn lan_address(stack: Stack<'static>) -> LocalAddress {
    Box::new(move || {
        stack
            .config_v4()
            .map(|config| format!("{}", config.address.address()))
    })
}

/// Serves the channel's routes: the configuration path with the Gateway's
/// shared `/status`, `/mode`, and `/owners` below it, and the webhook, whose
/// longer prefix takes its own requests.
fn serve_routes<Storage: PluginStorage>(
    webserver: &WebServer,
    channel: Rc<BlueBubblesChannel<Storage>>,
) -> Result<[WebRouteRegistration; 2], WebServerError> {
    let config = ConfigEndpoint(Rc::clone(&channel));
    let gateway = Rc::clone(&channel.gateway);
    let shared = gateway.serve_channel(CONFIG_API_PATH, Rc::clone(&channel), config)?;
    let webhook = webserver.serve_http_prefix(WEBHOOK_API_PATH, WebhookEndpoint(channel))?;
    Ok([shared, webhook])
}

/// The channel's portal card status.
fn entry_status(status: ChannelStatus) -> EntryStatus {
    let state = match status.level {
        ChannelLevel::Off => EntryState::Off,
        ChannelLevel::Ready => EntryState::Ready,
        ChannelLevel::Attention => EntryState::Attention,
    };
    EntryStatus::new(
        state,
        WebText {
            zh: status.zh,
            en: status.en,
        },
    )
}

#[embassy_executor::task]
async fn bluebubbles_receive_task(runtime: ReceiveRuntime, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime).await;
    log::info!("stopped BlueBubbles receive task");
}

#[derive(Debug, thiserror::Error)]
#[error("BlueBubbles receive runtime was not prepared during Plugin registration")]
struct ReceiveRuntimeUnavailable;

/// `POST` on [`CONFIG_API_PATH`] stores a configuration; the channel
/// status is under `/status`.
struct ConfigEndpoint<Storage>(Rc<BlueBubblesChannel<Storage>>);

fn response(status: u16, body: &'static [u8]) -> HttpResponse {
    HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
}

impl<Storage: PluginStorage> ConfigEndpoint<Storage> {
    async fn configure(&self, body: &[u8]) -> HttpResponse {
        let Ok(config) = serde_json::from_slice::<ConfigRequest>(body) else {
            log::warn!("rejected invalid BlueBubbles gateway configuration");
            return response(400, br#"{"error":"invalid_request"}"#);
        };
        let channel = &*self.0;
        match channel.configure(config).await {
            Ok(()) => {
                channel.gateway.sync_receive(channel).ok();
                channel.receive().restart();
                response(204, b"")
            }
            Err(ConfigureError::Storage) => response(500, br#"{"error":"storage"}"#),
            Err(ConfigureError::Registration) => {
                channel.gateway.sync_receive(channel).ok();
                response(422, br#"{"error":"registration_failed"}"#)
            }
        }
    }
}

impl<Storage: PluginStorage> HttpEndpoint for ConfigEndpoint<Storage> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Post => self.configure(request.body()).await,
                _ => response(405, br#"{"error":"method_not_allowed"}"#),
            }
        })
    }
}

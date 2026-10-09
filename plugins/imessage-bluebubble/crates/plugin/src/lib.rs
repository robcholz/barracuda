//! IMessage BlueBubbles provider Plugin.
//!
//! Sends through the BlueBubbles REST API and, in `send_receive`, receives by
//! a webhook the BlueBubbles server posts to the device's own webserver.

#![no_std]

extern crate alloc;

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

use barracuda_captive_portal_plugin::{CaptivePortal, ResourceFiles, WebEntry, WebGroup, WebText};
use barracuda_imessage_gateway_channel::{
    entry_status, receive_runtime, status_response, sync_receive, ChannelControl, ChannelEndpoint,
    ReceiveRuntime, ReceiveTiming, JSON_CONTENT_TYPE,
};
use barracuda_imessage_gateway_plugin::IMessageGateway;
use barracuda_plugin::api::{PluginContext, SharedEntropy};
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginStorage,
    PluginTaskToken,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer, WEB_SERVER_PORT,
};
use embassy_futures::select::select;
use embassy_net::Stack;
use http_client::embedded_nal_async::{Dns, TcpConnect};
use http_client::ClientFactory;

use crate::channel::{
    BlueBubblesChannel, ChannelSetup, ConfigRequest, ConfigureError, InboundSink, LocalAddress,
};
use crate::receive::Receiver;
use crate::state::HOOK_PATH;
use crate::webhook::WebhookEndpoint;

/// HTTP path accepting BlueBubbles configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/bluebubbles";
/// HTTP path of the channel mode.
pub const MODE_API_PATH: &str = "/api/gateway/bluebubbles/mode";
/// HTTP path of the allowed accounts and the pairing code.
pub const OWNERS_API_PATH: &str = "/api/gateway/bluebubbles/owners";
/// HTTP path prefix of the webhook; the secret follows it.
pub const WEBHOOK_API_PATH: &str = HOOK_PATH;

/// Plugin that exposes BlueBubbles configuration, registers the resulting
/// channel, and receives its messages by webhook.
#[barracuda_plugin::macros::plugin]
pub struct IMessageBlueBubblePlugin {
    http_clients: ClientFactory<'static>,
    stack: Stack<'static>,
    entropy: SharedEntropy,
    runtime: Option<ReceiveRuntime>,
}

impl IMessageBlueBubblePlugin {
    /// Creates an unconfigured provider using Platform HTTP resources, the
    /// IP stack whose address the webhook names, and the Platform entropy.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
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
        let stack = self.stack;
        let address: LocalAddress = Box::new(move || {
            stack
                .config_v4()
                .map(|config| format!("{}", config.address.address()))
        });
        let runtime = register_channel(
            context,
            self.http_clients.clone(),
            self.entropy.clone(),
            address,
        )?;
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

/// Builds the channel and registers its portal entry and endpoints;
/// returns the receive runtime for the Plugin's task.
fn register_channel<Storage, T, D>(
    context: &mut PluginRegisterContext<'_, Storage>,
    http_clients: ClientFactory<'static, T, D>,
    entropy: SharedEntropy,
    address: LocalAddress,
) -> PluginResult<ReceiveRuntime>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    let portal = context.require::<CaptivePortal>("captive-portal")?;
    let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
    let webserver = context.require::<WebServer>("webserver")?;
    let inbound: Rc<dyn InboundSink> = gateway.clone();
    let channel = Rc::new(embassy_futures::block_on(BlueBubblesChannel::load(
        ChannelSetup {
            gateway,
            inbound,
            http_clients,
            storage: context.storage().clone(),
            entropy,
            address,
            port: WEB_SERVER_PORT,
        },
    ))?);
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
                move || entry_status(&*status),
            )
            .map_err(PluginError::registration)?,
    );
    let runtime = receive_runtime(
        channel.receive_control(),
        Rc::new(Receiver(Rc::clone(&channel))),
        ReceiveTiming::DEVICE,
    );
    // Webhook receiving holds no slot, so this never reports `no_slot`.
    sync_receive(&*channel).ok();
    // One route serves the configuration path and its `/mode` and `/owners`;
    // the longer webhook prefix takes its own requests.
    let routes = [
        webserver.serve_http_prefix(
            CONFIG_API_PATH,
            ChannelEndpoint::new(
                Rc::clone(&channel),
                CONFIG_API_PATH,
                ConfigEndpoint(Rc::clone(&channel)),
            ),
        ),
        webserver.serve_http_prefix(WEBHOOK_API_PATH, WebhookEndpoint(channel)),
    ];
    for route in routes {
        context.retain(route.map_err(PluginError::registration)?);
    }
    Ok(runtime)
}

#[embassy_executor::task]
async fn bluebubbles_receive_task(runtime: ReceiveRuntime, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime).await;
    log::info!("stopped BlueBubbles receive task");
}

#[derive(Debug, thiserror::Error)]
#[error("BlueBubbles receive runtime was not prepared during Plugin registration")]
struct ReceiveRuntimeUnavailable;

/// `GET` and `POST` on [`CONFIG_API_PATH`].
struct ConfigEndpoint<Storage, T: 'static, D: 'static>(Rc<BlueBubblesChannel<Storage, T, D>>);

impl<Storage, T, D> ConfigEndpoint<Storage, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    fn response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }

    /// The shared channel status plus the webhook counters while receiving:
    /// `"webhook":{"lost":n,"skipped":n}`.
    fn status(&self) -> HttpResponse {
        let channel = &*self.0;
        let response = status_response(channel);
        let Some(shared) = response
            .body()
            .filter(|_| channel.mode().receives())
            .and_then(|body| body.strip_suffix(b"}"))
        else {
            return response;
        };
        let mut body = Vec::from(shared);
        body.extend_from_slice(
            format!(
                r#","webhook":{{"lost":{},"skipped":{}}}}}"#,
                channel.book.lost(),
                channel.book.skipped()
            )
            .as_bytes(),
        );
        HttpResponse::new(200, JSON_CONTENT_TYPE, body)
    }

    async fn configure(&self, body: &[u8]) -> HttpResponse {
        let Ok(config) = serde_json::from_slice::<ConfigRequest>(body) else {
            log::warn!("rejected invalid BlueBubbles gateway configuration");
            return Self::response(400, br#"{"error":"invalid_request"}"#);
        };
        match self.0.configure(config).await {
            Ok(()) => {
                sync_receive(&*self.0).ok();
                self.0.receive().restart();
                HttpResponse::new(204, JSON_CONTENT_TYPE, Vec::new())
            }
            Err(ConfigureError::Storage) => Self::response(500, br#"{"error":"storage"}"#),
            Err(ConfigureError::Registration) => {
                sync_receive(&*self.0).ok();
                Self::response(422, br#"{"error":"registration_failed"}"#)
            }
        }
    }
}

impl<Storage, T, D> HttpEndpoint for ConfigEndpoint<Storage, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Get => self.status(),
                HttpMethod::Post => self.configure(request.body()).await,
                _ => Self::response(405, br#"{"error":"method_not_allowed"}"#),
            }
        })
    }
}

//! IMessage Telegram provider Plugin.

#![no_std]

extern crate alloc;

mod channel;
mod receive;
#[cfg(test)]
mod tests;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_captive_portal_plugin::{CaptivePortal, ResourceFiles, WebEntry, WebGroup, WebText};
use barracuda_imessage_gateway_channel::{
    entry_status, status_response, ChannelEndpoint, PairingEntropy, ReceiveRuntime, ReceiveTiming,
};
use barracuda_imessage_gateway_plugin::IMessageGateway;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginStorage,
    PluginTaskToken,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use embassy_futures::select::select;
use http_client::{ClientFactory, ReceiveSlots};
use serde::{Deserialize, Serialize};
use telegram::TelegramConfig;

use crate::channel::{build_channel, ConfigureError, TelegramChannel};

/// HTTP path accepting Telegram configuration and reporting channel status.
pub const CONFIG_API_PATH: &str = "/api/gateway/telegram";
/// HTTP path setting the channel mode.
pub const MODE_API_PATH: &str = "/api/gateway/telegram/mode";
/// HTTP path listing owners and the pairing code.
pub const OWNERS_API_PATH: &str = "/api/gateway/telegram/owners";

/// Gateway channel name.
const CHANNEL: &str = "telegram";
const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Plugin that exposes Telegram configuration, registers the resulting
/// channel, and receives its messages.
#[barracuda_plugin::macros::plugin]
pub struct IMessageTelegramPlugin {
    http_clients: ClientFactory<'static>,
    receive_slots: ReceiveSlots,
    entropy: PairingEntropy,
    runtime: Option<ReceiveRuntime>,
}

impl IMessageTelegramPlugin {
    /// Creates an unconfigured provider using Platform HTTP resources, the
    /// receive slots, and the Platform entropy for pairing codes.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
            receive_slots: context.receive_slots.clone(),
            entropy: PairingEntropy::new(context.entropy.clone()),
            runtime: None,
        }
    }
}

impl Plugin for IMessageTelegramPlugin {
    const REQUIREMENTS: barracuda_plugin::manager::PluginRequirements =
        barracuda_plugin::manager::PluginRequirements::new()
            .with_filesystem(barracuda_plugin::manager::PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let portal = context.require::<CaptivePortal>("captive-portal")?;
        let gateway = context.require::<IMessageGateway>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let webserver = context.require::<WebServer>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[1],
        )?;
        let (channel, runtime) = build_channel(
            context.storage().clone(),
            gateway,
            self.http_clients.clone(),
            self.receive_slots.clone(),
            self.entropy.clone(),
            ReceiveTiming::DEVICE,
        )?;
        let status = Rc::clone(&channel);
        context.retain(
            portal
                .register_with_status(
                    WebEntry {
                        id: "imessage-telegram",
                        group: WebGroup::Channel,
                        order: 20,
                        title: WebText {
                            zh: "Telegram",
                            en: "Telegram",
                        },
                        summary: WebText {
                            zh: "通过 Bot 收发消息",
                            en: "Send and receive through a bot",
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
        // One route serves the configuration path and its `/mode` and `/owners`.
        let endpoint = ChannelEndpoint::new(
            Rc::clone(&channel),
            CONFIG_API_PATH,
            ConfigEndpoint { channel },
        );
        context.retain(
            webserver
                .serve_http_prefix(CONFIG_API_PATH, endpoint)
                .map_err(PluginError::registration)?,
        );
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
            .ok_or_else(|| PluginError::registration(ReceiveRuntimeUnavailable))?;
        let task = telegram_receive_task(runtime, context.task_token())
            .map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

/// Owns the receive loop; parked without a slot unless the channel is
/// configured and in `send_receive`.
#[embassy_executor::task]
async fn telegram_receive_task(runtime: ReceiveRuntime, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime).await;
    log::info!("stopped Telegram receive task");
}

#[derive(Debug, thiserror::Error)]
#[error("Telegram receive runtime was not prepared during Plugin registration")]
struct ReceiveRuntimeUnavailable;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigRequest {
    token: String,
    #[serde(default = "default_telegram_api_base")]
    api_base: String,
    #[serde(default = "default_draft_min_delta_bytes")]
    draft_min_delta_bytes: usize,
}

impl From<ConfigRequest> for TelegramConfig {
    fn from(value: ConfigRequest) -> Self {
        TelegramConfig {
            token: value.token,
            api_base: value.api_base,
            draft_min_delta_bytes: value.draft_min_delta_bytes,
        }
    }
}

fn default_telegram_api_base() -> String {
    "https://api.telegram.org".into()
}
const fn default_draft_min_delta_bytes() -> usize {
    24
}

/// `GET` reports the channel status; `POST` stores a configuration.
struct ConfigEndpoint<Storage, C: 'static, D: 'static> {
    channel: Rc<TelegramChannel<Storage, C, D>>,
}

fn response(status: u16, body: &'static [u8]) -> HttpResponse {
    HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
}

impl<Storage, C, D> HttpEndpoint for ConfigEndpoint<Storage, C, D>
where
    Storage: PluginStorage,
    C: http_client::embedded_nal_async::TcpConnect + 'static,
    D: http_client::embedded_nal_async::Dns + 'static,
{
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Post => {}
                HttpMethod::Get => return status_response(&*self.channel),
                _ => return response(405, br#"{"error":"method_not_allowed"}"#),
            }
            let Ok(config) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                log::warn!("rejected invalid Telegram gateway configuration");
                return response(400, br#"{"error":"invalid_request"}"#);
            };
            match self.channel.configure(config).await {
                Ok(()) => response(204, b""),
                Err(ConfigureError::Storage) => response(500, br#"{"error":"storage"}"#),
                Err(ConfigureError::Registration) => {
                    response(422, br#"{"error":"registration_failed"}"#)
                }
            }
        })
    }
}

fn encode_configuration(config: &ConfigRequest) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(config)
}

fn decode_configuration(bytes: &[u8]) -> Result<ConfigRequest, serde_json::Error> {
    serde_json::from_slice(bytes)
}

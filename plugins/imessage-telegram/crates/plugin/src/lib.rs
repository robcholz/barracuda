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

use barracuda_captive_portal_plugin::{
    CaptivePortal, EntryState, EntryStatus, ResourceFiles, WebEntry, WebGroup, WebText,
};
use barracuda_imessage_gateway_plugin::{
    ChannelLevel, ChannelStatus, IMessageGateway, ReceiveRuntime, ReceiveTiming,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginStorage,
    PluginTaskToken,
};
use barracuda_webserver_plugin::{HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse};
use embassy_futures::select::select;
use serde::{Deserialize, Serialize};
use telegram::TelegramConfig;

use crate::channel::{build_channel, ConfigureError, TelegramChannel};

/// HTTP path accepting Telegram configuration; the Gateway serves the
/// channel's `/status`, `/mode`, and `/owners` below it.
pub const CONFIG_API_PATH: &str = "/api/gateway/telegram";

/// Gateway channel name.
const CHANNEL: &str = "telegram";
const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Plugin that exposes Telegram configuration, registers the resulting
/// channel, and receives its messages.
#[barracuda_plugin::macros::plugin]
pub struct IMessageTelegramPlugin {
    runtime: Option<ReceiveRuntime>,
}

impl IMessageTelegramPlugin {
    /// Creates the unregistered Telegram channel Plugin.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { runtime: None }
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
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let (channel, runtime) = build_channel(
            context.storage().clone(),
            Rc::clone(&gateway),
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
                    move || entry_status(status.gateway.channel_status(&*status)),
                )
                .map_err(PluginError::registration)?,
        );
        let config = ConfigEndpoint {
            channel: Rc::clone(&channel),
        };
        context.retain(
            gateway
                .serve_channel(CONFIG_API_PATH, channel, config)
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

/// `POST` stores a configuration; the channel status is under `/status`.
struct ConfigEndpoint<Storage> {
    channel: Rc<TelegramChannel<Storage>>,
}

fn response(status: u16, body: &'static [u8]) -> HttpResponse {
    HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
}

impl<Storage: PluginStorage> HttpEndpoint for ConfigEndpoint<Storage> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Post => {}
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

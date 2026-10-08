//! IMessage Telegram provider Plugin.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_captive_portal_plugin::{CaptivePortal, ResourceFiles, WebEntry, WebGroup, WebText};
use barracuda_imessage_gateway_plugin::IMessageGateway;
use barracuda_imessage_gateway_plugin::{MessageChannel, MessageChannelRegistration};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use http_client::ClientFactory;
use serde::{Deserialize, Serialize};
use telegram::{Telegram, TelegramConfig};

/// HTTP path accepting Telegram configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/telegram";

const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Plugin that exposes Telegram configuration and registers the resulting channel.
#[barracuda_plugin::macros::plugin]
pub struct IMessageTelegramPlugin {
    http_clients: ClientFactory<'static>,
}

impl IMessageTelegramPlugin {
    /// Creates an unconfigured provider using Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
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
        context.retain(
            portal
                .register(
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
                )
                .map_err(PluginError::registration)?,
        );
        let gateway = context.require::<IMessageGateway>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let webserver = context.require::<WebServer>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[1],
        )?;
        let channel_registration =
            embassy_futures::block_on(load_configuration(context.storage()))?
                .map(|config| {
                    let channel: Rc<dyn MessageChannel> =
                        Rc::new(Telegram::new(self.http_clients.clone(), config.into()));
                    gateway.register(channel).map_err(PluginError::registration)
                })
                .transpose()?;
        let endpoint = ConfigEndpoint {
            gateway,
            http_clients: self.http_clients.clone(),
            channel_registration: Mutex::new(channel_registration),
            storage: context.storage().clone(),
        };
        let registration = webserver
            .serve_http(CONFIG_API_PATH, endpoint)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

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

struct ConfigEndpoint<Storage> {
    gateway: Rc<IMessageGateway>,
    http_clients: ClientFactory<'static>,
    channel_registration: Mutex<NoopRawMutex, Option<MessageChannelRegistration>>,
    storage: Storage,
}

impl<Storage> ConfigEndpoint<Storage> {
    fn response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }
}

impl<Storage: PluginStorage> HttpEndpoint for ConfigEndpoint<Storage> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Post {
                return Self::response(405, br#"{"error":"method_not_allowed"}"#);
            }
            let Ok(config) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                log::warn!("rejected invalid Telegram gateway configuration");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let mut channel_registration = self.channel_registration.lock().await;
            let previous_configuration =
                match self.storage.get_bytes(CONFIGURATION_STORAGE_KEY).await {
                    Ok(configuration) => configuration,
                    Err(error) => {
                        log::error!(
                            "failed to read the previous Telegram gateway configuration: {error}"
                        );
                        return Self::response(500, br#"{"error":"storage"}"#);
                    }
                };
            let Ok(bytes) = encode_configuration(&config) else {
                log::error!("failed to encode Telegram gateway configuration");
                return Self::response(500, br#"{"error":"storage"}"#);
            };
            if let Err(error) = self
                .storage
                .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
                .await
            {
                log::error!("failed to persist Telegram gateway configuration: {error}");
                return Self::response(500, br#"{"error":"storage"}"#);
            }
            let channel: Rc<dyn MessageChannel> =
                Rc::new(Telegram::new(self.http_clients.clone(), config.into()));
            channel_registration.take();
            match self.gateway.register(channel) {
                Ok(registration) => {
                    channel_registration.replace(registration);
                    log::info!("configured Telegram gateway provider");
                    Self::response(204, b"")
                }
                Err(error) => {
                    let restored = if let Some(previous) = previous_configuration.as_deref() {
                        self.storage.put(CONFIGURATION_STORAGE_KEY, previous).await
                    } else {
                        self.storage.delete(CONFIGURATION_STORAGE_KEY).await
                    };
                    if let Err(storage_error) = restored {
                        log::error!(
                            "failed to roll back rejected Telegram gateway configuration: {storage_error}"
                        );
                        return Self::response(500, br#"{"error":"storage"}"#);
                    }
                    log::warn!("rejected Telegram gateway configuration: {error}");
                    Self::response(422, br#"{"error":"invalid_configuration"}"#)
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

async fn load_configuration<Storage: PluginStorage>(
    storage: &Storage,
) -> PluginResult<Option<ConfigRequest>> {
    storage
        .get_bytes(CONFIGURATION_STORAGE_KEY)
        .await?
        .map(|bytes| decode_configuration(&bytes))
        .transpose()
        .map_err(PluginError::registration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_configuration_round_trips_every_field() -> Result<(), serde_json::Error> {
        let config = ConfigRequest {
            token: "secret".into(),
            api_base: "https://telegram.example".into(),
            draft_min_delta_bytes: 31,
        };

        let bytes = encode_configuration(&config)?;
        let restored = decode_configuration(&bytes)?;

        assert_eq!(restored.token, config.token);
        assert_eq!(restored.api_base, config.api_base);
        assert_eq!(restored.draft_min_delta_bytes, config.draft_min_delta_bytes);
        Ok(())
    }
}

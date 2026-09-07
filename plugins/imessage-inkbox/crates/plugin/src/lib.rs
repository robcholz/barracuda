//! IMessage Inkbox provider Plugin.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_captive_portal_plugin::{CaptivePortal, ResourceFiles, WebEntry};
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
use inkbox::{Inkbox, InkboxConfig};
use serde::{Deserialize, Serialize};

/// HTTP path accepting Inkbox configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/inkbox";

const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Plugin that exposes Inkbox configuration and registers the resulting channel.
#[barracuda_plugin::macros::plugin]
pub struct IMessageInkboxPlugin {
    http_clients: ClientFactory<'static>,
}

impl IMessageInkboxPlugin {
    /// Creates an unconfigured provider using Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
    }
}

impl Plugin for IMessageInkboxPlugin {
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
                        id: "imessage-inkbox",
                        title: "Inkbox",
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
                        Rc::new(Inkbox::new(self.http_clients.clone(), config.into()));
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
    api_key: String,
    identity_id: String,
    #[serde(default = "default_inkbox_api_base")]
    api_base: String,
}

impl From<ConfigRequest> for InkboxConfig {
    fn from(value: ConfigRequest) -> Self {
        InkboxConfig {
            api_key: value.api_key,
            identity_id: value.identity_id,
            api_base: value.api_base,
        }
    }
}

fn default_inkbox_api_base() -> String {
    "https://inkbox.ai".into()
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
                log::warn!("rejected invalid Inkbox gateway configuration");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let mut channel_registration = self.channel_registration.lock().await;
            let Ok(bytes) = encode_configuration(&config) else {
                log::error!("failed to encode Inkbox gateway configuration");
                return Self::response(500, br#"{"error":"storage"}"#);
            };
            if let Err(error) = self
                .storage
                .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
                .await
            {
                log::error!("failed to persist Inkbox gateway configuration: {error}");
                return Self::response(500, br#"{"error":"storage"}"#);
            }
            let channel: Rc<dyn MessageChannel> =
                Rc::new(Inkbox::new(self.http_clients.clone(), config.into()));
            channel_registration.take();
            match self.gateway.register(channel) {
                Ok(registration) => {
                    channel_registration.replace(registration);
                    log::info!("configured Inkbox gateway provider");
                    Self::response(204, b"")
                }
                Err(error) => {
                    log::warn!("rejected Inkbox gateway configuration: {error}");
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
            api_key: "secret".into(),
            identity_id: "identity".into(),
            api_base: "https://inkbox.example".into(),
        };

        let bytes = encode_configuration(&config)?;
        let restored = decode_configuration(&bytes)?;

        assert_eq!(restored.api_key, config.api_key);
        assert_eq!(restored.identity_id, config.identity_id);
        assert_eq!(restored.api_base, config.api_base);
        Ok(())
    }
}

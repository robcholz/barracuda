//! IMessage QQ provider Plugin.
#![no_std]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use barracuda_imessage_gateway_plugin::{IMessageGateway, PLUGIN_ID as IMESSAGE_GATEWAY_PLUGIN_ID};
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
    PLUGIN_ID as WEBSERVER_PLUGIN_ID,
};
use core::cell::RefCell;
use gateway::{MessageChannel, MessageChannelRegistration};
use http_client::ClientFactory;
use qq::{QQConfig, QQ};
use serde::Deserialize;

/// Stable identity of the IMessage QQ Plugin.
pub const PLUGIN_ID: &str = "imessage-qq";
/// HTTP path accepting QQ configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/qq";
const JSON_CONTENT_TYPE: &str = "application/json";

/// Plugin that exposes QQ configuration and registers the resulting channel.
pub struct IMessageQQPlugin {
    http_clients: ClientFactory<'static>,
}

impl IMessageQQPlugin {
    /// Creates an unconfigured provider using Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
    }
}

impl<const M: usize> Plugin<M> for IMessageQQPlugin {
    const DEPENDS_ON: &'static [&'static str] = &[IMESSAGE_GATEWAY_PLUGIN_ID, WEBSERVER_PLUGIN_ID];
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let endpoint = ConfigEndpoint {
            gateway: context.require::<IMessageGateway>(IMESSAGE_GATEWAY_PLUGIN_ID)?,
            http_clients: self.http_clients.clone(),
            channel_registration: RefCell::new(None),
        };
        let webserver = context.require::<WebServer>(WEBSERVER_PLUGIN_ID)?;
        let registration = webserver
            .serve_http(CONFIG_API_PATH, endpoint)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

#[derive(Deserialize)]
struct ConfigRequest {
    app_id: String,
    access_token: String,
    #[serde(default = "default_qq_api_base")]
    api_base: String,
}

impl From<ConfigRequest> for QQConfig {
    fn from(value: ConfigRequest) -> Self {
        QQConfig {
            app_id: value.app_id,
            access_token: value.access_token,
            api_base: value.api_base,
        }
    }
}
fn default_qq_api_base() -> String {
    "https://api.sgroup.qq.com".into()
}

struct ConfigEndpoint {
    gateway: Rc<IMessageGateway>,
    http_clients: ClientFactory<'static>,
    channel_registration: RefCell<Option<MessageChannelRegistration>>,
}
impl ConfigEndpoint {
    fn response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }
}
impl HttpEndpoint for ConfigEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Post {
                return Self::response(405, br#"{"error":"method_not_allowed"}"#);
            }
            let Ok(config) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                log::warn!("rejected invalid QQ gateway configuration");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let channel: Rc<dyn MessageChannel> =
                Rc::new(QQ::new(self.http_clients.clone(), config.into()));
            self.channel_registration.borrow_mut().take();
            match self.gateway.register(channel) {
                Ok(registration) => {
                    self.channel_registration.borrow_mut().replace(registration);
                    log::info!("configured QQ gateway provider");
                    Self::response(204, b"")
                }
                Err(error) => {
                    log::warn!("rejected QQ gateway configuration: {error}");
                    Self::response(422, br#"{"error":"invalid_configuration"}"#)
                }
            }
        })
    }
}

//! IMessage Telegram provider Plugin.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use barracuda_imessage_gateway_plugin::IMessageGateway;
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use gateway::{MessageChannel, MessageChannelRegistration};
use http_client::ClientFactory;
use serde::Deserialize;
use telegram::{Telegram, TelegramConfig};

/// HTTP path accepting Telegram configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/telegram";

const JSON_CONTENT_TYPE: &str = "application/json";

/// Plugin that exposes Telegram configuration and registers the resulting channel.
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

#[barracuda_plugin_api::plugin]
impl<const M: usize> Plugin<M> for IMessageTelegramPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let gateway = context.require::<IMessageGateway>(<Self as Plugin<M>>::DEPENDS_ON[0])?;
        let webserver = context.require::<WebServer>(<Self as Plugin<M>>::DEPENDS_ON[1])?;
        let endpoint = ConfigEndpoint {
            gateway,
            http_clients: self.http_clients.clone(),
            channel_registration: RefCell::new(None),
        };
        let registration = webserver
            .serve_http(CONFIG_API_PATH, endpoint)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

#[derive(Deserialize)]
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
                log::warn!("rejected invalid Telegram gateway configuration");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let channel: Rc<dyn MessageChannel> =
                Rc::new(Telegram::new(self.http_clients.clone(), config.into()));
            self.channel_registration.borrow_mut().take();
            match self.gateway.register(channel) {
                Ok(registration) => {
                    self.channel_registration.borrow_mut().replace(registration);
                    log::info!("configured Telegram gateway provider");
                    Self::response(204, b"")
                }
                Err(error) => {
                    log::warn!("rejected Telegram gateway configuration: {error}");
                    Self::response(422, br#"{"error":"invalid_configuration"}"#)
                }
            }
        })
    }
}

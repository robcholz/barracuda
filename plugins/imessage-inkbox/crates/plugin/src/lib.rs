//! IMessage Inkbox provider Plugin.

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
use inkbox::{Inkbox, InkboxConfig};
use serde::Deserialize;

/// HTTP path accepting Inkbox configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/inkbox";

const JSON_CONTENT_TYPE: &str = "application/json";

/// Plugin that exposes Inkbox configuration and registers the resulting channel.
#[barracuda_plugin_api::plugin]
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

impl<const M: usize> Plugin<M> for IMessageInkboxPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let gateway = context.require::<IMessageGateway>(
            <Self as barracuda_plugin_manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let webserver = context.require::<WebServer>(
            <Self as barracuda_plugin_manager::PluginDeclaration>::DEPENDS_ON[1],
        )?;
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
                log::warn!("rejected invalid Inkbox gateway configuration");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let channel: Rc<dyn MessageChannel> =
                Rc::new(Inkbox::new(self.http_clients.clone(), config.into()));
            self.channel_registration.borrow_mut().take();
            match self.gateway.register(channel) {
                Ok(registration) => {
                    self.channel_registration.borrow_mut().replace(registration);
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

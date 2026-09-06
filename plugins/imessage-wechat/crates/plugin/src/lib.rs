//! IMessage Wechat provider Plugin.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use barracuda_imessage_gateway_plugin::IMessageGateway;
use barracuda_imessage_gateway_plugin::{MessageChannel, MessageChannelRegistration};
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use http_client::ClientFactory;
use serde::Deserialize;
use wechat::{Wechat, WechatConfig};

/// HTTP path accepting Wechat configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/wechat";

const JSON_CONTENT_TYPE: &str = "application/json";

/// Plugin that exposes Wechat configuration and registers the resulting channel.
#[barracuda_plugin_api::plugin]
pub struct IMessageWechatPlugin {
    http_clients: ClientFactory<'static>,
}

impl IMessageWechatPlugin {
    /// Creates an unconfigured provider using Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
    }
}

impl<const M: usize> Plugin<M> for IMessageWechatPlugin {
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
    token: String,
    #[serde(default = "default_wechat_api_base")]
    api_base: String,
    #[serde(default = "default_app_id")]
    app_id: String,
    #[serde(default = "default_client_version")]
    client_version: String,
    #[serde(default)]
    route_tag: Option<String>,
    #[serde(default = "default_x_wechat_uin")]
    x_wechat_uin: String,
}

impl From<ConfigRequest> for WechatConfig {
    fn from(value: ConfigRequest) -> Self {
        WechatConfig {
            token: value.token,
            api_base: value.api_base,
            app_id: value.app_id,
            client_version: value.client_version,
            route_tag: value.route_tag,
            x_wechat_uin: value.x_wechat_uin,
        }
    }
}

fn default_wechat_api_base() -> String {
    "https://ilinkai.weixin.qq.com".into()
}
fn default_app_id() -> String {
    "bot".into()
}
fn default_client_version() -> String {
    "131329".into()
}
fn default_x_wechat_uin() -> String {
    "MA==".into()
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
                log::warn!("rejected invalid Wechat gateway configuration");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let channel: Rc<dyn MessageChannel> =
                Rc::new(Wechat::new(self.http_clients.clone(), config.into()));
            self.channel_registration.borrow_mut().take();
            match self.gateway.register(channel) {
                Ok(registration) => {
                    self.channel_registration.borrow_mut().replace(registration);
                    log::info!("configured Wechat gateway provider");
                    Self::response(204, b"")
                }
                Err(error) => {
                    log::warn!("rejected Wechat gateway configuration: {error}");
                    Self::response(422, br#"{"error":"invalid_configuration"}"#)
                }
            }
        })
    }
}

//! IMessage Wechat provider Plugin.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use barracuda_imessage_gateway_plugin::{IMessageGateway, PLUGIN_ID as IMESSAGE_GATEWAY_PLUGIN_ID};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginError, PluginResult};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
    PLUGIN_ID as WEBSERVER_PLUGIN_ID,
};
use gateway::{MessageChannel, MessageChannelRegistration};
use http_client::HttpClient;
use serde::Deserialize;
use wechat::{Wechat, WechatConfig};

/// Stable identity of the IMessage Wechat Plugin.
pub const PLUGIN_ID: &str = "imessage-wechat";
/// HTTP path accepting Wechat configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/wechat";

const JSON_CONTENT_TYPE: &str = "application/json";

/// Plugin that exposes Wechat configuration and registers the resulting channel.
pub struct IMessageWechatPlugin {
    http: Rc<dyn HttpClient>,
}

impl IMessageWechatPlugin {
    /// Creates an unconfigured provider using Barracuda's shared HTTP client.
    #[must_use]
    pub fn new(http: Rc<dyn HttpClient>) -> Self {
        Self { http }
    }
}

impl<const M: usize> Plugin<M> for IMessageWechatPlugin {
    const DEPENDS_ON: &'static [&'static str] = &[IMESSAGE_GATEWAY_PLUGIN_ID, WEBSERVER_PLUGIN_ID];

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(&mut self, context: &mut PluginContext<'_, M, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let gateway = context.require::<IMessageGateway>(IMESSAGE_GATEWAY_PLUGIN_ID)?;
        let webserver = context.require::<WebServer>(WEBSERVER_PLUGIN_ID)?;
        let endpoint = ConfigEndpoint {
            gateway,
            http: Rc::clone(&self.http),
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
    fn from(self: ConfigRequest) -> Self {
        WechatConfig {
            token: self.token,
            api_base: self.api_base,
            app_id: self.app_id,
            client_version: self.client_version,
            route_tag: self.route_tag,
            x_wechat_uin: self.x_wechat_uin,
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
    http: Rc<dyn HttpClient>,
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
                Rc::new(Wechat::new(Rc::clone(&self.http), config.into()));
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

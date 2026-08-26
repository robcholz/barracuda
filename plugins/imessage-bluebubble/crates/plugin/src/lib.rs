//! IMessage BlueBubbles provider Plugin.

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
use bluebubbles::{BlueBubbles, BlueBubblesConfig};
use gateway::{MessageChannel, MessageChannelRegistration};
use http_client::ClientFactory;
use serde::Deserialize;

/// Stable identity of the IMessage BlueBubbles Plugin.
pub const PLUGIN_ID: &str = "imessage-bluebubble";
/// HTTP path accepting BlueBubbles configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/bluebubbles";

const JSON_CONTENT_TYPE: &str = "application/json";

/// Plugin that exposes BlueBubbles configuration and registers the resulting channel.
pub struct IMessageBlueBubblePlugin {
    http_clients: ClientFactory<'static>,
}

impl IMessageBlueBubblePlugin {
    /// Creates an unconfigured provider using Platform HTTP resources.
    #[must_use]
    pub fn new(http_clients: ClientFactory<'static>) -> Self {
        Self { http_clients }
    }
}

impl<const M: usize> Plugin<M> for IMessageBlueBubblePlugin {
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
    server_url: String,
    password: String,
    #[serde(default = "default_true")]
    use_private_api: bool,
    #[serde(default = "default_stream_edit_min_delta_bytes")]
    stream_edit_min_delta_bytes: usize,
    #[serde(default = "default_stream_max_edits")]
    stream_max_edits: usize,
}

impl From<ConfigRequest> for BlueBubblesConfig {
    fn from(value: ConfigRequest) -> Self {
        BlueBubblesConfig {
            server_url: value.server_url,
            password: value.password,
            use_private_api: value.use_private_api,
            stream_edit_min_delta_bytes: value.stream_edit_min_delta_bytes,
            stream_max_edits: value.stream_max_edits,
        }
    }
}

const fn default_true() -> bool {
    true
}
const fn default_stream_edit_min_delta_bytes() -> usize {
    128
}
const fn default_stream_max_edits() -> usize {
    4
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
                log::warn!("rejected invalid BlueBubbles gateway configuration");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let channel: Rc<dyn MessageChannel> =
                Rc::new(BlueBubbles::new(self.http_clients.clone(), config.into()));
            self.channel_registration.borrow_mut().take();
            match self.gateway.register(channel) {
                Ok(registration) => {
                    self.channel_registration.borrow_mut().replace(registration);
                    log::info!("configured BlueBubbles gateway provider");
                    Self::response(204, b"")
                }
                Err(error) => {
                    log::warn!("rejected BlueBubbles gateway configuration: {error}");
                    Self::response(422, br#"{"error":"invalid_configuration"}"#)
                }
            }
        })
    }
}

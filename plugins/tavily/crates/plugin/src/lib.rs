//! Tavily web-search Plugin with dynamic Agent RPC and HTTP configuration.
#![no_std]

extern crate alloc;

mod component;

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use core::cell::RefCell;

use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse,
    PLUGIN_ID as WEBSERVER_PLUGIN_ID, WebServer,
};
use component::{TavilyComponent, TavilyConfig};
use http_client::ClientFactory;
use serde::Deserialize;

pub use barracuda_tavily_wire::{TavilySearchError, TavilySearchRequest, TavilySearchResult};
pub use component::TavilySearch;

/// Stable Tavily Plugin identity.
pub const PLUGIN_ID: &str = "tavily";
/// HTTP endpoint accepting Tavily credentials.
pub const CONFIG_API_PATH: &str = "/api/tavily";
const JSON_CONTENT_TYPE: &str = "application/json";
const DEFAULT_API_BASE: &str = "https://api.tavily.com";

/// Plugin providing Tavily search to Agents through Event Router.
pub struct TavilyPlugin {
    http_clients: ClientFactory<'static>,
}

impl TavilyPlugin {
    /// Creates a Tavily Plugin using the shared Platform HTTP service.
    #[must_use]
    pub fn new(context: &PluginContext) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
    }
}

impl<const M: usize> Plugin<M> for TavilyPlugin {
    const DEPENDS_ON: &'static [&'static str] = &[WEBSERVER_PLUGIN_ID];

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
        let webserver = context.require::<WebServer>(WEBSERVER_PLUGIN_ID)?;
        let config = Rc::new(RefCell::new(None));
        context.event_router.load(TavilyComponent::new(
            Rc::clone(&config),
            self.http_clients.clone(),
        ))?;
        let registration = webserver
            .serve_http(CONFIG_API_PATH, ConfigEndpoint { config })
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigRequest {
    api_key: String,
    #[serde(default = "default_api_base")]
    api_base: String,
}

fn default_api_base() -> String {
    DEFAULT_API_BASE.into()
}

struct ConfigEndpoint {
    config: Rc<RefCell<Option<TavilyConfig>>>,
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
            let Ok(request) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            if request.api_key.trim().is_empty()
                || !(request.api_base.starts_with("https://")
                    || request.api_base.starts_with("http://"))
            {
                return Self::response(422, br#"{"error":"invalid_configuration"}"#);
            }
            self.config.replace(Some(TavilyConfig {
                api_key: request.api_key,
                api_base: request.api_base,
            }));
            log::info!("configured Tavily web search");
            Self::response(204, b"")
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_to_tavily_api() {
        let request: ConfigRequest = serde_json::from_slice(br#"{"api_key":"secret"}"#).unwrap();
        assert_eq!(request.api_base, DEFAULT_API_BASE);
    }
}

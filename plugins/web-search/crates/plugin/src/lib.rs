//! Web Search Plugin backed by Tavily with dynamic Agent RPC configuration.
#![no_std]

extern crate alloc;

mod component;

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use core::cell::RefCell;

use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use component::{TavilyConfig, WebSearchComponent};
use http_client::ClientFactory;
use serde::Deserialize;

pub use barracuda_web_search_wire::{WebSearchError, WebSearchRequest, WebSearchResult};
pub use component::WebSearch;

/// HTTP endpoint accepting Tavily credentials.
pub const CONFIG_API_PATH: &str = "/api/tavily";
const JSON_CONTENT_TYPE: &str = "application/json";
const DEFAULT_API_BASE: &str = "https://api.tavily.com";

/// Plugin providing web search to Agents through Event Router.
pub struct WebSearchPlugin {
    http_clients: ClientFactory<'static>,
}

impl WebSearchPlugin {
    /// Creates a Web Search Plugin using the shared Platform HTTP service.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
    }
}

#[barracuda_plugin_api::plugin]
impl<const M: usize> Plugin<M> for WebSearchPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let webserver = context.require::<WebServer>(<Self as Plugin<M>>::DEPENDS_ON[0])?;
        let config = Rc::new(RefCell::new(None));
        context.event_router.load(WebSearchComponent::new(
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
            log::info!("configured Tavily provider for web search");
            Self::response(204, b"")
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use barracuda_event_router::RpcMethod;

    #[test]
    fn exposes_web_search_identity() {
        let plugin = WebSearchPlugin::new(&mut PluginContext::new(
            barracuda_platform_test::never_embassy_stack(),
            ClientFactory::plaintext(barracuda_platform_test::never_embassy_stack()),
        ));
        assert_eq!(Plugin::<512>::id(&plugin), "web-search");
        assert_eq!(WebSearch::ADDRESS, "web_search.search");
    }

    #[test]
    fn config_defaults_to_tavily_api() {
        let request: ConfigRequest = serde_json::from_slice(br#"{"api_key":"secret"}"#).unwrap();
        assert_eq!(request.api_base, DEFAULT_API_BASE);
    }
}

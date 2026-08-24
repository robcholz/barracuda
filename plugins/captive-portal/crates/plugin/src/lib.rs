//! Captive Portal Plugin exposing Agent model API configuration over HTTP.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use barracuda_agent_plugin::{AgentSetApi, ApiPurpose, BackendKind, ModelApiConfig};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginError, PluginStartFuture};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use serde::Deserialize;

/// Stable identity of the Agent Plugin dependency.
pub const AGENT_PLUGIN_ID: &str = barracuda_agent_plugin::PLUGIN_ID;
/// Stable identity of the WebServer Plugin dependency.
pub const WEBSERVER_PLUGIN_ID: &str = barracuda_webserver_plugin::PLUGIN_ID;
/// Stable identity of the Captive Portal Plugin.
pub const PLUGIN_ID: &str = "captive-portal";
/// HTTP path accepting Agent model API configuration.
pub const SET_API_PATH: &str = "/api/model-api";

const JSON_CONTENT_TYPE: &str = "application/json";

/// Plugin that mounts Agent model API configuration on the shared WebServer.
#[derive(Default)]
pub struct CaptivePortalPlugin;

impl CaptivePortalPlugin {
    /// Creates the Captive Portal Plugin.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl<const M: usize> Plugin<M> for CaptivePortalPlugin {
    const DEPENDS_ON: &'static [&'static str] = &[AGENT_PLUGIN_ID, WEBSERVER_PLUGIN_ID];

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn start<'a, Storage>(
        &'a mut self,
        context: &'a mut PluginContext<'_, M, Storage>,
    ) -> PluginStartFuture<'a>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        Box::pin(async move {
            let agent = context.require::<AgentSetApi>(AGENT_PLUGIN_ID)?;
            let webserver = context.require::<WebServer>(WEBSERVER_PLUGIN_ID)?;
            let registration = webserver
                .serve_http(SET_API_PATH, SetApiEndpoint::new(agent))
                .map_err(PluginError::registration)?;
            context.retain(registration);
            Ok(())
        })
    }
}

#[derive(Deserialize)]
struct SetApiRequest {
    timeout_ms: u32,
    max_tokens: u32,
    image_max_bytes: usize,
    backend: SetApiBackend,
    purpose: SetApiPurpose,
    default: bool,
    api_key: alloc::string::String,
    model: alloc::string::String,
    base_url: alloc::string::String,
}

impl SetApiRequest {
    fn into_parts(self) -> (ModelApiConfig, ApiPurpose, bool) {
        let mut api =
            ModelApiConfig::new(self.backend.into(), self.api_key, self.model, self.base_url);
        api.timeout_ms = self.timeout_ms;
        api.max_tokens = self.max_tokens;
        api.image_max_bytes = self.image_max_bytes;
        (api, self.purpose.into(), self.default)
    }
}

#[derive(Deserialize)]
enum SetApiBackend {
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    #[serde(rename = "anthropic_compatible")]
    AnthropicCompatible,
}

impl From<SetApiBackend> for BackendKind {
    fn from(value: SetApiBackend) -> Self {
        match value {
            SetApiBackend::OpenAiCompatible => Self::OpenAiCompatible,
            SetApiBackend::AnthropicCompatible => Self::AnthropicCompatible,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SetApiPurpose {
    RootAgent,
    SubAgent,
    Memory,
    Compaction,
}

impl From<SetApiPurpose> for ApiPurpose {
    fn from(value: SetApiPurpose) -> Self {
        match value {
            SetApiPurpose::RootAgent => Self::RootAgent,
            SetApiPurpose::SubAgent => Self::SubAgent,
            SetApiPurpose::Memory => Self::Memory,
            SetApiPurpose::Compaction => Self::Compaction,
        }
    }
}

struct SetApiEndpoint {
    agent: Rc<AgentSetApi>,
}

impl SetApiEndpoint {
    const fn new(agent: Rc<AgentSetApi>) -> Self {
        Self { agent }
    }

    fn response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }
}

impl HttpEndpoint for SetApiEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Post {
                return Self::response(405, br#"{"error":"method_not_allowed"}"#);
            }
            let Ok(request) = serde_json::from_slice::<SetApiRequest>(request.body()) else {
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let (api, purpose, default) = request.into_parts();
            match self.agent.set_api(api, purpose, default) {
                Ok(()) => Self::response(204, b""),
                Err(_error) => Self::response(422, br#"{"error":"invalid_configuration"}"#),
            }
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use alloc::rc::Rc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use barracuda_agent_plugin::{AgentSetApi, InitError};
    use barracuda_plugin_manager::Plugin;
    use barracuda_webserver_plugin::{HttpEndpoint, HttpMethod, HttpRequest};

    use super::{
        ApiPurpose, CaptivePortalPlugin, SetApiEndpoint, AGENT_PLUGIN_ID, PLUGIN_ID, SET_API_PATH,
    };

    const VALID_JSON: &[u8] = br#"{
        "timeout_ms": 30000,
        "max_tokens": 4096,
        "image_max_bytes": 1048576,
        "backend": "openai_compatible",
        "purpose": "root_agent",
        "default": true,
        "api_key": "secret",
        "model": "test-model",
        "base_url": "https://example.invalid/v1"
    }"#;

    #[test]
    fn plugin_identity_and_dependencies_are_stable() {
        let plugin = CaptivePortalPlugin::new();

        assert_eq!(Plugin::<512>::id(&plugin), PLUGIN_ID);
        assert_eq!(
            <CaptivePortalPlugin as Plugin<512>>::DEPENDS_ON,
            &[AGENT_PLUGIN_ID, "webserver"]
        );
        assert_eq!(SET_API_PATH, "/api/model-api");
    }

    #[test]
    fn post_sets_the_supplied_model_api() {
        let observed = Rc::new(RefCell::new(None));
        let target = Rc::clone(&observed);
        let endpoint =
            SetApiEndpoint::new(Rc::new(AgentSetApi::new(move |api, purpose, default| {
                *target.borrow_mut() = Some((api, purpose, default));
                Ok(())
            })));

        let response = futures_lite::future::block_on(
            endpoint.handle(HttpRequest::new(HttpMethod::Post, VALID_JSON.to_vec())),
        );

        assert_eq!(response.status(), 204);
        assert!(response.body().is_empty());
        let request = observed.borrow();
        let (api, purpose, default) = request.as_ref().expect("set API request received");
        assert_eq!(api.model, "test-model");
        assert_eq!(api.api_key, "secret");
        assert_eq!(*purpose, ApiPurpose::RootAgent);
        assert!(*default);
    }

    #[test]
    fn malformed_json_is_rejected() {
        let endpoint =
            SetApiEndpoint::new(Rc::new(AgentSetApi::new(|_api, _purpose, _default| Ok(()))));

        let response = futures_lite::future::block_on(
            endpoint.handle(HttpRequest::new(HttpMethod::Post, vec![b'{'])),
        );

        assert_eq!(response.status(), 400);
        assert_eq!(response.body(), br#"{"error":"invalid_request"}"#);
    }

    #[test]
    fn invalid_model_configuration_is_rejected() {
        let endpoint =
            SetApiEndpoint::new(Rc::new(AgentSetApi::new(|_api, _purpose, _default| {
                Err(InitError::MissingApiKey)
            })));

        let response = futures_lite::future::block_on(
            endpoint.handle(HttpRequest::new(HttpMethod::Post, VALID_JSON.to_vec())),
        );

        assert_eq!(response.status(), 422);
        assert_eq!(response.body(), br#"{"error":"invalid_configuration"}"#);
    }

    #[test]
    fn non_post_methods_are_rejected_without_setting() {
        let called = Rc::new(RefCell::new(false));
        let target = Rc::clone(&called);
        let endpoint = SetApiEndpoint::new(Rc::new(AgentSetApi::new(
            move |_api, _purpose, _default| {
                *target.borrow_mut() = true;
                Ok(())
            },
        )));

        let response = futures_lite::future::block_on(
            endpoint.handle(HttpRequest::new(HttpMethod::Get, Vec::new())),
        );

        assert_eq!(response.status(), 405);
        assert!(!*called.borrow());
    }
}

use alloc::{boxed::Box, rc::Rc, vec::Vec};

use barracuda_agent_runtime::{AgentRuntime, ApiPurpose};
use barracuda_model_api::{BackendKind, InitError, ModelApiConfig};
use barracuda_webserver_plugin::{HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse};
use serde::Deserialize;

/// HTTP path accepting Agent model API configurations.
pub const SET_API_PATH: &str = "/api/model-api";

const JSON_CONTENT_TYPE: &str = "application/json";

pub(crate) struct SetApiEndpoint {
    set_api: Box<SetApiHandler>,
}

type SetApiHandler = dyn Fn(ModelApiConfig, ApiPurpose, bool) -> Result<(), InitError>;

impl SetApiEndpoint {
    pub(crate) fn new(runtime: Rc<AgentRuntime>) -> Self {
        Self::with_handler(move |api, purpose, default| runtime.set_api(api, purpose, default))
    }

    fn with_handler(
        handler: impl Fn(ModelApiConfig, ApiPurpose, bool) -> Result<(), InitError> + 'static,
    ) -> Self {
        Self {
            set_api: Box::new(handler),
        }
    }

    fn response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }
}

impl HttpEndpoint for SetApiEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Post {
                log::debug!("rejected non-POST model API configuration request");
                return Self::response(405, br#"{"error":"method_not_allowed"}"#);
            }
            let Ok(requests) = serde_json::from_slice::<Vec<SetApiRequest>>(request.body()) else {
                log::warn!("rejected invalid model API configuration request");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            if requests.is_empty() {
                log::warn!("rejected empty model API configuration request");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            }
            for request in requests {
                let (api, purpose, default) = request.into_parts();
                if let Err(error) = (self.set_api)(api, purpose, default) {
                    log::warn!("rejected model API configuration for {purpose:?}: {error}");
                    return Self::response(422, br#"{"error":"invalid_configuration"}"#);
                }
                log::info!("configured model API for {purpose:?}, default={default}");
            }
            Self::response(204, b"")
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::{rc::Rc, vec, vec::Vec};
    use core::cell::RefCell;

    use barracuda_model_api::InitError;
    use barracuda_webserver_plugin::{HttpEndpoint, HttpMethod, HttpRequest};

    use super::{ApiPurpose, SetApiEndpoint};

    const VALID_JSON: &[u8] = br#"[
        {
            "timeout_ms": 30000,
            "max_tokens": 4096,
            "image_max_bytes": 1048576,
            "backend": "openai_compatible",
            "purpose": "root_agent",
            "default": true,
            "api_key": "secret",
            "model": "root-model",
            "base_url": "https://example.invalid/v1"
        },
        {
            "timeout_ms": 30000,
            "max_tokens": 2048,
            "image_max_bytes": 1048576,
            "backend": "anthropic_compatible",
            "purpose": "memory",
            "default": false,
            "api_key": "secret",
            "model": "memory-model",
            "base_url": "https://example.invalid/v1"
        }
    ]"#;

    #[test]
    fn post_sets_the_supplied_model_apis() {
        let observed = Rc::new(RefCell::new(Vec::new()));
        let target = Rc::clone(&observed);
        let endpoint = SetApiEndpoint::with_handler(move |api, purpose, default| {
            target.borrow_mut().push((api, purpose, default));
            Ok(())
        });

        let response = futures_lite::future::block_on(
            endpoint.handle(HttpRequest::new(HttpMethod::Post, VALID_JSON.to_vec())),
        );

        assert_eq!(response.status(), 204);
        assert!(response.body().is_empty());
        let requests = observed.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].0.model, "root-model");
        assert_eq!(requests[0].0.api_key, "secret");
        assert_eq!(requests[0].1, ApiPurpose::RootAgent);
        assert!(requests[0].2);
        assert_eq!(requests[1].0.model, "memory-model");
        assert_eq!(requests[1].1, ApiPurpose::Memory);
        assert!(!requests[1].2);
    }

    #[test]
    fn malformed_or_empty_batches_are_rejected() {
        let endpoint = SetApiEndpoint::with_handler(|_api, _purpose, _default| Ok(()));

        for body in [vec![b'{'], b"[]".to_vec()] {
            let response = futures_lite::future::block_on(
                endpoint.handle(HttpRequest::new(HttpMethod::Post, body)),
            );
            assert_eq!(response.status(), 400);
            assert_eq!(response.body(), br#"{"error":"invalid_request"}"#);
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let endpoint = SetApiEndpoint::with_handler(|_api, _purpose, _default| Ok(()));
        let body = br#"[{"timeout_ms":1,"max_tokens":1,"image_max_bytes":1,"backend":"openai_compatible","purpose":"root_agent","default":true,"api_key":"secret","model":"model","base_url":"https://example.invalid/v1","extra":true}]"#;

        let response = futures_lite::future::block_on(
            endpoint.handle(HttpRequest::new(HttpMethod::Post, body.to_vec())),
        );

        assert_eq!(response.status(), 400);
        assert_eq!(response.body(), br#"{"error":"invalid_request"}"#);
    }

    #[test]
    fn invalid_model_configuration_is_rejected() {
        let endpoint =
            SetApiEndpoint::with_handler(|_api, _purpose, _default| Err(InitError::MissingApiKey));

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
        let endpoint = SetApiEndpoint::with_handler(move |_api, _purpose, _default| {
            *target.borrow_mut() = true;
            Ok(())
        });

        let response = futures_lite::future::block_on(
            endpoint.handle(HttpRequest::new(HttpMethod::Get, Vec::new())),
        );

        assert_eq!(response.status(), 405);
        assert_eq!(response.body(), br#"{"error":"method_not_allowed"}"#);
        assert!(!*called.borrow());
    }
}

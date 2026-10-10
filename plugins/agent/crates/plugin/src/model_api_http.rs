use alloc::{boxed::Box, rc::Rc, vec::Vec};
use core::{cell::Cell, future::Future, pin::Pin};

use barracuda_agent_runtime::{AgentRuntime, ApiPurpose, ModelApiManager};
use barracuda_model_api::{BackendKind, InitError, ModelApiConfig};
use barracuda_plugin::manager::{
    PluginError, PluginResult, PluginStorage, PluginWriteTransaction, StorageError,
};
use barracuda_webserver_plugin::{HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use serde::{Deserialize, Serialize};

/// HTTP path accepting Agent model API configurations.
pub const SET_API_PATH: &str = "/api/model-api";
/// HTTP path reporting the active model API configuration without keys.
pub const STATUS_API_PATH: &str = "/api/model-api/status";

const JSON_CONTENT_TYPE: &str = "application/json";
const DEFAULT_STORAGE_KEY: &str = "default";
const COMPACTION_STORAGE_KEY: &str = "purpose.compaction";
const MEMORY_STORAGE_KEY: &str = "purpose.memory";
const ROOT_AGENT_STORAGE_KEY: &str = "purpose.root_agent";
const SUB_AGENT_STORAGE_KEY: &str = "purpose.sub_agent";

pub(crate) struct SetApiEndpoint {
    set_api: Box<SetApiHandler>,
    configuration: Rc<Mutex<NoopRawMutex, ModelApiManager>>,
    persistence: Box<dyn ConfigurationPersistence>,
    /// Whether the active configuration has at least one model, for the portal status.
    configured: Rc<Cell<bool>>,
}

/// Returns whether `configuration` resolves a model for any purpose.
pub(crate) fn has_model(configuration: &ModelApiManager) -> bool {
    [
        ApiPurpose::RootAgent,
        ApiPurpose::SubAgent,
        ApiPurpose::Memory,
        ApiPurpose::Compaction,
    ]
    .into_iter()
    .any(|purpose| configuration.get_api(purpose).is_some())
}

type SetApiHandler = dyn Fn(ModelApiConfig, ApiPurpose, bool) -> Result<(), InitError>;

impl SetApiEndpoint {
    pub(crate) fn new<Storage: PluginStorage>(
        runtime: Rc<AgentRuntime>,
        storage: Storage,
        configuration: ModelApiManager,
        configured: Rc<Cell<bool>>,
    ) -> Self {
        configured.set(has_model(&configuration));
        Self {
            set_api: Box::new(move |api, purpose, default| runtime.set_api(api, purpose, default)),
            configuration: Rc::new(Mutex::new(configuration)),
            persistence: Box::new(PluginConfigurationPersistence(storage)),
            configured,
        }
    }

    #[cfg(test)]
    fn with_handler(
        handler: impl Fn(ModelApiConfig, ApiPurpose, bool) -> Result<(), InitError> + 'static,
    ) -> Self {
        Self::with_handler_and_persistence(
            handler,
            NoopConfigurationPersistence,
            ModelApiManager::default(),
        )
    }

    #[cfg(test)]
    fn with_handler_and_persistence(
        handler: impl Fn(ModelApiConfig, ApiPurpose, bool) -> Result<(), InitError> + 'static,
        persistence: impl ConfigurationPersistence + 'static,
        configuration: ModelApiManager,
    ) -> Self {
        Self {
            set_api: Box::new(handler),
            configuration: Rc::new(Mutex::new(configuration)),
            persistence: Box::new(persistence),
            configured: Rc::new(Cell::new(false)),
        }
    }

    fn response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }

    /// The [`STATUS_API_PATH`] endpoint over this endpoint's configuration.
    pub(crate) fn status_endpoint(&self) -> StatusEndpoint {
        StatusEndpoint(Rc::clone(&self.configuration))
    }
}

/// `GET` [`STATUS_API_PATH`]:
/// `{"configured":bool,"default":Model|null,"purposes":{"root_agent":Model|null,"sub_agent":…,"memory":…,"compaction":…}}`
/// with `Model` = `{"backend":"openai_compatible"|"anthropic_compatible","model":"…","base_url":"…"}`.
/// A purpose is `null` when it has no model of its own and uses the default.
/// API keys are never reported.
pub(crate) struct StatusEndpoint(Rc<Mutex<NoopRawMutex, ModelApiManager>>);

#[derive(Serialize)]
struct StatusBody {
    configured: bool,
    default: Option<StatusModel>,
    purposes: StatusPurposes,
}

#[derive(Serialize)]
struct StatusPurposes {
    root_agent: Option<StatusModel>,
    sub_agent: Option<StatusModel>,
    memory: Option<StatusModel>,
    compaction: Option<StatusModel>,
}

#[derive(Serialize)]
struct StatusModel {
    backend: BackendKind,
    model: alloc::string::String,
    base_url: alloc::string::String,
}

impl StatusModel {
    /// The reported fields of `api`; its key and limits are left out.
    fn from_api(api: ModelApiConfig) -> Self {
        Self {
            backend: api.backend,
            model: api.model,
            base_url: api.base_url,
        }
    }
}

impl StatusBody {
    fn from_configuration(configuration: &ModelApiManager) -> Self {
        let explicit = |purpose| {
            configuration
                .get_explicit_api(purpose)
                .map(StatusModel::from_api)
        };
        Self {
            configured: has_model(configuration),
            default: configuration.get_default_api().map(StatusModel::from_api),
            purposes: StatusPurposes {
                root_agent: explicit(ApiPurpose::RootAgent),
                sub_agent: explicit(ApiPurpose::SubAgent),
                memory: explicit(ApiPurpose::Memory),
                compaction: explicit(ApiPurpose::Compaction),
            },
        }
    }
}

impl HttpEndpoint for StatusEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Get {
                return SetApiEndpoint::response(405, br#"{"error":"method_not_allowed"}"#);
            }
            let body = StatusBody::from_configuration(&*self.0.lock().await);
            match serde_json::to_vec(&body) {
                Ok(body) => HttpResponse::new(200, JSON_CONTENT_TYPE, body),
                Err(error) => {
                    log::error!("failed to encode the model API status: {error}");
                    SetApiEndpoint::response(500, br#"{"error":"encoding"}"#)
                }
            }
        })
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
            let mut current = self.configuration.lock().await;
            let mut configuration = current.clone();
            for request in &requests {
                let (api, purpose, default) = request.clone().into_parts();
                if let Err(error) = configuration.set_api(api, purpose, default) {
                    log::warn!("rejected model API configuration for {purpose:?}: {error}");
                    return Self::response(422, br#"{"error":"invalid_configuration"}"#);
                }
            }
            if let Err(error) = self.persistence.persist(&configuration).await {
                log::error!("failed to persist model API configuration: {error}");
                return Self::response(500, br#"{"error":"storage"}"#);
            }
            for request in requests {
                let (api, purpose, default) = request.into_parts();
                if let Err(error) = (self.set_api)(api, purpose, default) {
                    log::warn!("rejected model API configuration for {purpose:?}: {error}");
                    return Self::response(422, br#"{"error":"invalid_configuration"}"#);
                }
                log::info!("configured model API for {purpose:?}, default={default}");
            }
            self.configured.set(has_model(&configuration));
            *current = configuration;
            Self::response(204, b"")
        })
    }
}

#[derive(Clone, Deserialize)]
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

type PersistenceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), ConfigurationStorageError>> + 'a>>;

trait ConfigurationPersistence {
    fn persist<'a>(&'a self, configuration: &'a ModelApiManager) -> PersistenceFuture<'a>;
}

struct PluginConfigurationPersistence<Storage>(Storage);

impl<Storage: PluginStorage> ConfigurationPersistence for PluginConfigurationPersistence<Storage> {
    fn persist<'a>(&'a self, configuration: &'a ModelApiManager) -> PersistenceFuture<'a> {
        Box::pin(async move { persist_configuration(&self.0, configuration).await })
    }
}

#[cfg(test)]
struct NoopConfigurationPersistence;

#[cfg(test)]
impl ConfigurationPersistence for NoopConfigurationPersistence {
    fn persist<'a>(&'a self, _configuration: &'a ModelApiManager) -> PersistenceFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, thiserror::Error)]
enum ConfigurationStorageError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Codec(#[from] serde_json::Error),
}

async fn persist_configuration<Storage: PluginStorage>(
    storage: &Storage,
    configuration: &ModelApiManager,
) -> Result<(), ConfigurationStorageError> {
    let records = [
        (DEFAULT_STORAGE_KEY, configuration.get_default_api()),
        (
            COMPACTION_STORAGE_KEY,
            configuration.get_explicit_api(ApiPurpose::Compaction),
        ),
        (
            MEMORY_STORAGE_KEY,
            configuration.get_explicit_api(ApiPurpose::Memory),
        ),
        (
            ROOT_AGENT_STORAGE_KEY,
            configuration.get_explicit_api(ApiPurpose::RootAgent),
        ),
        (
            SUB_AGENT_STORAGE_KEY,
            configuration.get_explicit_api(ApiPurpose::SubAgent),
        ),
    ];
    let encoded = records
        .into_iter()
        .map(|(key, config)| {
            config
                .map(|config| serde_json::to_vec(&config))
                .transpose()
                .map(|bytes| (key, bytes))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut transaction = storage.write_transaction().await;
    for (key, bytes) in &encoded {
        if let Some(bytes) = bytes {
            transaction.write(key, bytes.as_slice()).await?;
        }
    }
    transaction.commit().await?;
    Ok(())
}

pub(crate) async fn load_configuration<Storage: PluginStorage>(
    storage: &Storage,
) -> PluginResult<ModelApiManager> {
    let mut configuration = ModelApiManager::default();
    for (key, purpose) in [
        (COMPACTION_STORAGE_KEY, ApiPurpose::Compaction),
        (MEMORY_STORAGE_KEY, ApiPurpose::Memory),
        (ROOT_AGENT_STORAGE_KEY, ApiPurpose::RootAgent),
        (SUB_AGENT_STORAGE_KEY, ApiPurpose::SubAgent),
    ] {
        if let Some(bytes) = storage.get_bytes(key).await? {
            let api = serde_json::from_slice(&bytes).map_err(PluginError::registration)?;
            configuration
                .set_api(api, purpose, false)
                .map_err(PluginError::registration)?;
        }
    }
    if let Some(bytes) = storage.get_bytes(DEFAULT_STORAGE_KEY).await? {
        let api = serde_json::from_slice(&bytes).map_err(PluginError::registration)?;
        configuration
            .set_default_api(api)
            .map_err(PluginError::registration)?;
    }
    Ok(configuration)
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

#[derive(Clone, Deserialize)]
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

#[derive(Clone, Deserialize)]
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

    use alloc::{boxed::Box, rc::Rc, vec, vec::Vec};
    use core::cell::RefCell;

    use barracuda_model_api::InitError;
    use barracuda_platform_test::{memory_partition, MemoryPartition};
    use barracuda_plugin::manager::{
        Plugin, PluginDeclaration, PluginError, PluginManager, PluginRegisterContext, PluginResult,
    };
    use barracuda_webserver_plugin::{HttpEndpoint, HttpMethod, HttpRequest};

    use super::{ApiPurpose, SetApiEndpoint};

    struct RecordingPersistence(Rc<RefCell<Option<barracuda_agent_runtime::ModelApiManager>>>);

    impl super::ConfigurationPersistence for RecordingPersistence {
        fn persist<'a>(
            &'a self,
            configuration: &'a barracuda_agent_runtime::ModelApiManager,
        ) -> super::PersistenceFuture<'a> {
            self.0.replace(Some(configuration.clone()));
            Box::pin(async { Ok(()) })
        }
    }

    struct FailingPersistence;

    impl super::ConfigurationPersistence for FailingPersistence {
        fn persist<'a>(
            &'a self,
            _configuration: &'a barracuda_agent_runtime::ModelApiManager,
        ) -> super::PersistenceFuture<'a> {
            Box::pin(async {
                Err(super::ConfigurationStorageError::Storage(
                    barracuda_plugin::manager::StorageError::KeyTooLong { max: 0 },
                ))
            })
        }
    }

    struct ConfigurationRoundTripPlugin {
        configuration: barracuda_agent_runtime::ModelApiManager,
        observed: Rc<RefCell<Option<barracuda_agent_runtime::ModelApiManager>>>,
    }

    impl PluginDeclaration for ConfigurationRoundTripPlugin {
        const ID: &'static str = "model-configuration-round-trip";
    }

    impl Plugin for ConfigurationRoundTripPlugin {
        fn register<Storage>(
            &mut self,
            context: &mut PluginRegisterContext<'_, Storage>,
        ) -> PluginResult<()>
        where
            Storage: barracuda_plugin::manager::PluginStorage,
        {
            futures_lite::future::block_on(async {
                super::persist_configuration(context.storage(), &self.configuration)
                    .await
                    .map_err(PluginError::registration)?;
                self.observed
                    .replace(Some(super::load_configuration(context.storage()).await?));
                Ok(())
            })
        }
    }

    fn plugin_manager() -> PluginManager<MemoryPartition> {
        futures_lite::future::block_on(async {
            let partition = memory_partition(64 * 1024)
                .await
                .expect("create test database region");
            PluginManager::open(partition)
                .await
                .expect("open Plugin storage")
        })
    }

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
        assert_eq!(response.body(), Some(&b""[..]));
        assert!(endpoint.configured.get());
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
    fn posts_persist_the_complete_accumulated_model_configuration() {
        let persisted = Rc::new(RefCell::new(None));
        let endpoint = SetApiEndpoint::with_handler_and_persistence(
            |_api, _purpose, _default| Ok(()),
            RecordingPersistence(Rc::clone(&persisted)),
            barracuda_agent_runtime::ModelApiManager::default(),
        );
        let first = br#"[{"timeout_ms":1,"max_tokens":2,"image_max_bytes":3,"backend":"openai_compatible","purpose":"root_agent","default":true,"api_key":"root-key","model":"root","base_url":"https://root.example/v1"}]"#;
        let second = br#"[{"timeout_ms":4,"max_tokens":5,"image_max_bytes":6,"backend":"anthropic_compatible","purpose":"memory","default":false,"api_key":"memory-key","model":"memory","base_url":"https://memory.example/v1"}]"#;

        for body in [first.as_slice(), second.as_slice()] {
            let response = futures_lite::future::block_on(
                endpoint.handle(HttpRequest::new(HttpMethod::Post, body.to_vec())),
            );
            assert_eq!(response.status(), 204);
        }

        let configuration = persisted.borrow().clone().expect("persisted configuration");
        assert_eq!(
            configuration
                .get_api(ApiPurpose::RootAgent)
                .expect("root configuration")
                .model,
            "root"
        );
        assert_eq!(
            configuration
                .get_api(ApiPurpose::Memory)
                .expect("memory configuration")
                .model,
            "memory"
        );
        assert_eq!(
            configuration
                .get_api(ApiPurpose::Compaction)
                .expect("default configuration")
                .model,
            "root"
        );
    }

    #[test]
    fn storage_failure_does_not_activate_model_configuration() {
        let called = Rc::new(RefCell::new(false));
        let observed = Rc::clone(&called);
        let endpoint = SetApiEndpoint::with_handler_and_persistence(
            move |_api, _purpose, _default| {
                observed.replace(true);
                Ok(())
            },
            FailingPersistence,
            barracuda_agent_runtime::ModelApiManager::default(),
        );

        let response = futures_lite::future::block_on(
            endpoint.handle(HttpRequest::new(HttpMethod::Post, VALID_JSON.to_vec())),
        );

        assert_eq!(response.status(), 500);
        assert_eq!(response.body(), Some(&br#"{"error":"storage"}"#[..]));
        assert!(!*called.borrow());
        assert!(!endpoint.configured.get());
    }

    #[test]
    fn model_configuration_round_trips_through_plugin_kv() {
        let mut configuration = barracuda_agent_runtime::ModelApiManager::default();
        let mut root = barracuda_model_api::ModelApiConfig::new(
            barracuda_model_api::BackendKind::OpenAiCompatible,
            "root-key",
            "root",
            "https://root.example/v1",
        );
        root.max_tokens = 1234;
        configuration
            .set_api(root, ApiPurpose::RootAgent, true)
            .expect("set root API");
        let memory = barracuda_model_api::ModelApiConfig::new(
            barracuda_model_api::BackendKind::AnthropicCompatible,
            "memory-key",
            "memory",
            "https://memory.example/v1",
        );
        configuration
            .set_api(memory, ApiPurpose::Memory, false)
            .expect("set memory API");
        let observed = Rc::new(RefCell::new(None));
        let mut manager = plugin_manager();

        manager
            .register(ConfigurationRoundTripPlugin {
                configuration,
                observed: Rc::clone(&observed),
            })
            .expect("round-trip model configuration");

        let restored = observed.borrow();
        let restored = restored.as_ref().expect("restored configuration");
        assert_eq!(
            restored
                .get_explicit_api(ApiPurpose::RootAgent)
                .expect("root binding")
                .max_tokens,
            1234
        );
        assert_eq!(
            restored
                .get_explicit_api(ApiPurpose::Memory)
                .expect("memory binding")
                .model,
            "memory"
        );
        assert_eq!(
            restored.get_default_api().expect("default API").model,
            "root"
        );
    }

    #[test]
    fn malformed_or_empty_batches_are_rejected() {
        let endpoint = SetApiEndpoint::with_handler(|_api, _purpose, _default| Ok(()));

        for body in [vec![b'{'], b"[]".to_vec()] {
            let response = futures_lite::future::block_on(
                endpoint.handle(HttpRequest::new(HttpMethod::Post, body)),
            );
            assert_eq!(response.status(), 400);
            assert_eq!(
                response.body(),
                Some(&br#"{"error":"invalid_request"}"#[..])
            );
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
        assert_eq!(
            response.body(),
            Some(&br#"{"error":"invalid_request"}"#[..])
        );
    }

    #[test]
    fn invalid_model_configuration_is_rejected() {
        let endpoint =
            SetApiEndpoint::with_handler(|_api, _purpose, _default| Err(InitError::MissingApiKey));

        let response = futures_lite::future::block_on(
            endpoint.handle(HttpRequest::new(HttpMethod::Post, VALID_JSON.to_vec())),
        );

        assert_eq!(response.status(), 422);
        assert_eq!(
            response.body(),
            Some(&br#"{"error":"invalid_configuration"}"#[..])
        );
        assert!(!endpoint.configured.get());
    }

    #[test]
    fn has_model_counts_explicit_and_default_bindings() {
        let api = |model: &str| {
            barracuda_model_api::ModelApiConfig::new(
                barracuda_model_api::BackendKind::OpenAiCompatible,
                "key",
                model,
                "https://example.invalid/v1",
            )
        };
        let mut configuration = barracuda_agent_runtime::ModelApiManager::default();
        assert!(!super::has_model(&configuration));
        configuration
            .set_api(api("memory"), ApiPurpose::Memory, false)
            .expect("set memory API");
        assert!(super::has_model(&configuration));
        let mut configuration = barracuda_agent_runtime::ModelApiManager::default();
        configuration
            .set_default_api(api("default"))
            .expect("set default API");
        assert!(super::has_model(&configuration));
    }

    #[test]
    fn a_base_url_that_is_not_an_http_url_is_rejected_before_it_is_used() {
        let called = Rc::new(RefCell::new(false));
        let target = Rc::clone(&called);
        let endpoint = SetApiEndpoint::with_handler(move |_api, _purpose, _default| {
            *target.borrow_mut() = true;
            Ok(())
        });

        for base_url in ["not a url", "ftp://example.invalid/v1", "https://"] {
            let body = alloc::format!(
                r#"[{{"timeout_ms":1,"max_tokens":1,"image_max_bytes":1,"backend":"openai_compatible","purpose":"root_agent","default":true,"api_key":"secret","model":"model","base_url":"{base_url}"}}]"#
            );
            let response = futures_lite::future::block_on(
                endpoint.handle(HttpRequest::new(HttpMethod::Post, body.into_bytes())),
            );
            assert_eq!(response.status(), 422, "{base_url}");
        }
        assert!(!*called.borrow());
    }

    #[test]
    fn status_reports_the_models_without_keys() {
        let endpoint = SetApiEndpoint::with_handler(|_api, _purpose, _default| Ok(()));
        let status = endpoint.status_endpoint();
        let get = || {
            let response = futures_lite::future::block_on(
                status.handle(HttpRequest::new(HttpMethod::Get, Vec::new())),
            );
            assert_eq!(response.status(), 200);
            alloc::string::String::from_utf8(response.body().expect("buffered").to_vec())
                .expect("UTF-8")
        };
        assert_eq!(
            get(),
            r#"{"configured":false,"default":null,"purposes":{"root_agent":null,"sub_agent":null,"memory":null,"compaction":null}}"#
        );

        let response = futures_lite::future::block_on(
            endpoint.handle(HttpRequest::new(HttpMethod::Post, VALID_JSON.to_vec())),
        );
        assert_eq!(response.status(), 204);
        let root = r#"{"backend":"openai_compatible","model":"root-model","base_url":"https://example.invalid/v1"}"#;
        let memory = r#"{"backend":"anthropic_compatible","model":"memory-model","base_url":"https://example.invalid/v1"}"#;
        let reported = get();
        assert_eq!(
            reported,
            alloc::format!(
                r#"{{"configured":true,"default":{root},"purposes":{{"root_agent":{root},"sub_agent":null,"memory":{memory},"compaction":null}}}}"#
            )
        );
        assert!(!reported.contains("secret"));

        let response = futures_lite::future::block_on(
            status.handle(HttpRequest::new(HttpMethod::Post, Vec::new())),
        );
        assert_eq!(response.status(), 405);
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
        assert_eq!(
            response.body(),
            Some(&br#"{"error":"method_not_allowed"}"#[..])
        );
        assert!(!*called.borrow());
    }
}

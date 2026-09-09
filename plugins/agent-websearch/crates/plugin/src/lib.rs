//! Tavily-backed Web Search Tool for the Agent runtime.
#![no_std]

extern crate alloc;

mod search;

use alloc::{boxed::Box, format, rc::Rc, string::String, vec::Vec};
use core::{cell::RefCell, fmt};

use barracuda_agent_plugin::{
    AgentToolRegistry,
    tools::{Tool, ToolGroup},
};
use barracuda_captive_portal_plugin::{CaptivePortal, ResourceFiles, WebEntry};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginEntryIterator, PluginError, PluginReadTransaction, PluginRegisterContext,
    PluginResult, PluginStorage, PluginWriteTransaction, StorageResult,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use http_client::ClientFactory;
use search::{TavilyConfig, WebSearchTool};
use serde::Deserialize;

/// HTTP endpoint accepting Tavily credentials.
pub const CONFIG_API_PATH: &str = "/api/tavily";
const JSON_CONTENT_TYPE: &str = "application/json";
const DEFAULT_API_BASE: &str = "https://api.tavily.com";
const API_BASE_STORAGE_KEY: &str = "api_base";
const API_KEY_STORAGE_KEY: &str = "api_key";

/// Plugin providing Tavily-backed Web Search directly to Agents.
#[barracuda_plugin::macros::plugin]
pub struct AgentWebsearchPlugin {
    http_clients: ClientFactory<'static>,
}

impl AgentWebsearchPlugin {
    /// Creates an Agent Web Search Plugin using the shared Platform HTTP service.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
    }
}

impl Plugin for AgentWebsearchPlugin {
    const REQUIREMENTS: barracuda_plugin::manager::PluginRequirements =
        barracuda_plugin::manager::PluginRequirements::new()
            .with_filesystem(barracuda_plugin::manager::PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let portal = context.require::<CaptivePortal>("captive-portal")?;
        context.retain(
            portal
                .register(
                    WebEntry {
                        id: "agent-websearch",
                        title: "网页搜索",
                        module: "entry.js",
                    },
                    ResourceFiles::from(context.filesystem()?.clone()),
                )
                .map_err(PluginError::registration)?,
        );
        let tools = context.require::<AgentToolRegistry>("agent")?;
        let webserver = context.require::<WebServer>("webserver")?;
        let config = Rc::new(RefCell::new(
            embassy_futures::block_on(load_configuration(context.storage()))?.map(Rc::new),
        ));
        tools
            .register_group(ToolGroup::new(
                "websearch",
                false,
                [Tool::new(WebSearchTool::new(
                    Rc::clone(&config),
                    self.http_clients.clone(),
                ))],
            ))
            .map_err(PluginError::registration)?;
        let registration = webserver
            .serve_http(
                CONFIG_API_PATH,
                ConfigEndpoint {
                    config,
                    storage: context.storage().clone(),
                },
            )
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

struct ConfigEndpoint<Storage> {
    config: Rc<RefCell<Option<Rc<TavilyConfig>>>>,
    storage: Storage,
}

impl<Storage> ConfigEndpoint<Storage> {
    fn response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }
}

impl<Storage: PluginStorage> HttpEndpoint for ConfigEndpoint<Storage> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Post {
                return Self::response(405, br#"{"error":"method_not_allowed"}"#);
            }
            let Ok(request) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            if !valid_configuration(&request.api_key, &request.api_base) {
                return Self::response(422, br#"{"error":"invalid_configuration"}"#);
            }
            if let Err(error) = persist_configuration(&self.storage, &request).await {
                log::error!("failed to persist Tavily configuration: {error}");
                return Self::response(500, br#"{"error":"storage"}"#);
            }
            let search_url = search_url(&request.api_base);
            self.config.replace(Some(Rc::new(TavilyConfig {
                api_key: request.api_key,
                search_url,
            })));
            log::info!("configured Tavily provider for web search");
            Self::response(204, b"")
        })
    }
}

fn valid_configuration(api_key: &str, api_base: &str) -> bool {
    !api_key.trim().is_empty()
        && api_key.bytes().all(|byte| byte.is_ascii_graphic())
        && api_base.bytes().all(|byte| byte.is_ascii_graphic())
        && (api_base.starts_with("https://") || api_base.starts_with("http://"))
}

fn search_url(api_base: &str) -> String {
    format!("{}/search", api_base.trim_end_matches('/'))
}

async fn persist_configuration<Storage: PluginStorage>(
    storage: &Storage,
    request: &ConfigRequest,
) -> StorageResult<()> {
    let mut transaction = storage.write_transaction().await;
    transaction
        .write(API_BASE_STORAGE_KEY, request.api_base.as_str())
        .await?;
    transaction
        .write(API_KEY_STORAGE_KEY, request.api_key.as_str())
        .await?;
    transaction.commit().await
}

async fn load_configuration<Storage: PluginStorage>(
    storage: &Storage,
) -> PluginResult<Option<TavilyConfig>> {
    let read = storage.read_transaction().await;
    let mut entries = read.entries().await?;
    let mut stored = StoredConfiguration::default();
    while let Some(entry) = entries.next().await? {
        stored
            .insert(entry.key(), entry.value_bytes())
            .map_err(PluginError::registration)?;
    }
    stored.finish().map_err(PluginError::registration)
}

#[derive(Default)]
struct StoredConfiguration {
    api_base: Option<String>,
    api_key: Option<String>,
}

impl StoredConfiguration {
    fn insert(&mut self, key: &str, value: &[u8]) -> Result<(), InvalidStoredConfiguration> {
        if key != API_BASE_STORAGE_KEY && key != API_KEY_STORAGE_KEY {
            return Ok(());
        }
        let value = core::str::from_utf8(value).map_err(|_error| InvalidStoredConfiguration)?;
        match key {
            API_BASE_STORAGE_KEY => self.api_base = Some(String::from(value)),
            API_KEY_STORAGE_KEY => self.api_key = Some(String::from(value)),
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<Option<TavilyConfig>, InvalidStoredConfiguration> {
        match (self.api_key, self.api_base) {
            (None, None) => Ok(None),
            (Some(api_key), Some(api_base)) if valid_configuration(&api_key, &api_base) => {
                Ok(Some(TavilyConfig {
                    api_key,
                    search_url: search_url(&api_base),
                }))
            }
            _ => Err(InvalidStoredConfiguration),
        }
    }
}

#[derive(Debug)]
struct InvalidStoredConfiguration;

impl fmt::Display for InvalidStoredConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("stored Tavily configuration is invalid")
    }
}

impl core::error::Error for InvalidStoredConfiguration {}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn exposes_agent_websearch_identity() {
        assert_eq!(
            <AgentWebsearchPlugin as barracuda_plugin::manager::PluginDeclaration>::ID,
            "agent-websearch"
        );
    }

    #[test]
    fn config_defaults_to_tavily_api() {
        let request: ConfigRequest = serde_json::from_slice(br#"{"api_key":"secret"}"#).unwrap();
        assert_eq!(request.api_base, DEFAULT_API_BASE);
    }

    #[test]
    fn configuration_has_no_plugin_specific_size_limit() {
        let api_key = "k".repeat(512);
        let api_base = format!("https://{}.example.com", "a".repeat(512));

        assert!(valid_configuration(&api_key, &api_base));
    }

    #[test]
    fn stored_utf8_entries_restore_tavily_configuration() {
        let mut stored = StoredConfiguration::default();
        stored.insert("future_binary_state", &[0xff]).unwrap();
        stored
            .insert(API_BASE_STORAGE_KEY, b"https://search.example.com/")
            .unwrap();
        stored.insert(API_KEY_STORAGE_KEY, b"secret").unwrap();

        let config = stored.finish().unwrap().unwrap();
        assert_eq!(config.api_key, "secret");
        assert_eq!(config.search_url, "https://search.example.com/search");
    }

    #[test]
    fn incomplete_stored_configuration_is_rejected() {
        let mut stored = StoredConfiguration::default();
        stored.insert(API_KEY_STORAGE_KEY, b"secret").unwrap();

        assert!(stored.finish().is_err());
    }
}

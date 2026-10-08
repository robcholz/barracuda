//! IMessage QQ provider Plugin.
#![no_std]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use barracuda_captive_portal_plugin::{
    CaptivePortal, EntryStatus, ResourceFiles, WebEntry, WebGroup, WebText,
};
use barracuda_imessage_gateway_plugin::IMessageGateway;
use barracuda_imessage_gateway_plugin::{MessageChannel, MessageChannelRegistration};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use core::cell::Cell;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use http_client::ClientFactory;
use qq::{QQConfig, TokenError, QQ};
use serde::{de::IgnoredAny, Deserialize, Serialize};

/// HTTP path accepting QQ configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/qq";
const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Plugin that exposes QQ configuration and registers the resulting channel.
#[barracuda_plugin::macros::plugin]
pub struct IMessageQQPlugin {
    http_clients: ClientFactory<'static>,
}

impl IMessageQQPlugin {
    /// Creates an unconfigured provider using Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
    }
}

impl Plugin for IMessageQQPlugin {
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
        let configured = Rc::new(Cell::new(false));
        let status = Rc::clone(&configured);
        context.retain(
            portal
                .register_with_status(
                    WebEntry {
                        id: "imessage-qq",
                        group: WebGroup::Channel,
                        order: 40,
                        title: WebText { zh: "QQ", en: "QQ" },
                        summary: WebText {
                            zh: "QQ Bot 消息通道",
                            en: "QQ bot message channel",
                        },
                        icon: Some("icon.svg"),
                        figure: None,
                        module: "entry.js",
                    },
                    ResourceFiles::from(context.filesystem()?.clone()),
                    move || EntryStatus::configured(status.get()),
                )
                .map_err(PluginError::registration)?,
        );
        let gateway = context.require::<IMessageGateway>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let channel_registration = embassy_futures::block_on(restore_channel(
            context.storage(),
            &gateway,
            &self.http_clients,
        ))?;
        configured.set(channel_registration.is_some());
        let endpoint = ConfigEndpoint {
            gateway,
            http_clients: self.http_clients.clone(),
            channel_registration: Mutex::new(channel_registration),
            configured,
            storage: context.storage().clone(),
        };
        let webserver = context.require::<WebServer>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[1],
        )?;
        let registration = webserver
            .serve_http(CONFIG_API_PATH, endpoint)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigRequest {
    app_id: String,
    app_secret: String,
    #[serde(default = "default_qq_api_base")]
    api_base: String,
    #[serde(default = "default_qq_token_url")]
    token_url: String,
}

impl From<ConfigRequest> for QQConfig {
    fn from(value: ConfigRequest) -> Self {
        QQConfig {
            app_id: value.app_id,
            app_secret: value.app_secret,
            api_base: value.api_base,
            token_url: value.token_url,
        }
    }
}
fn default_qq_api_base() -> String {
    qq::DEFAULT_API_BASE.into()
}
fn default_qq_token_url() -> String {
    qq::DEFAULT_TOKEN_URL.into()
}

/// Stored configuration written before the App Secret replaced the
/// short-lived `access_token`. Only its presence is detected.
#[derive(Deserialize)]
struct LegacyConfiguration {
    #[serde(rename = "access_token")]
    _access_token: IgnoredAny,
}

/// JSON error body: `{"error":"<kind>","message":"<text>","code":"<upstream code>"}`.
#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
}

struct ConfigEndpoint<Storage, T: 'static = http_client::Tcp, D: 'static = http_client::Resolver> {
    gateway: Rc<IMessageGateway>,
    http_clients: ClientFactory<'static, T, D>,
    channel_registration: Mutex<NoopRawMutex, Option<MessageChannelRegistration>>,
    /// Whether a channel is registered with the Gateway, readable without the lock.
    configured: Rc<Cell<bool>>,
    storage: Storage,
}
impl<Storage, T: 'static, D: 'static> ConfigEndpoint<Storage, T, D> {
    fn response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }

    /// `GET` body: whether a channel is configured, never its settings.
    fn configured_response(configured: bool) -> HttpResponse {
        if configured {
            Self::response(200, br#"{"configured":true}"#)
        } else {
            Self::response(200, br#"{"configured":false}"#)
        }
    }

    fn error_response(status: u16, body: &ErrorBody<'_>) -> HttpResponse {
        match serde_json::to_vec(body) {
            Ok(bytes) => HttpResponse::new(status, JSON_CONTENT_TYPE, bytes),
            Err(_) => Self::response(500, br#"{"error":"internal"}"#),
        }
    }

    fn token_error_response(error: &TokenError) -> HttpResponse {
        match error {
            TokenError::Rejected { code, message } => {
                log::warn!("QQ rejected the gateway credentials: {error}");
                Self::error_response(
                    422,
                    &ErrorBody {
                        error: "verification_failed",
                        message: message.as_deref(),
                        code: code.as_deref(),
                    },
                )
            }
            TokenError::Unavailable { message } => {
                log::warn!("could not verify the QQ gateway credentials: {error}");
                Self::error_response(
                    502,
                    &ErrorBody {
                        error: "upstream_unavailable",
                        message: Some(message),
                        code: None,
                    },
                )
            }
        }
    }
}
impl<Storage, T, D> HttpEndpoint for ConfigEndpoint<Storage, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Post => {}
                HttpMethod::Get => return Self::configured_response(self.configured.get()),
                _ => return Self::response(405, br#"{"error":"method_not_allowed"}"#),
            }
            let Ok(config) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                log::warn!("rejected invalid QQ gateway configuration");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let Ok(bytes) = encode_configuration(&config) else {
                log::error!("failed to encode QQ gateway configuration");
                return Self::response(500, br#"{"error":"storage"}"#);
            };
            let qq = Rc::new(QQ::new(self.http_clients.clone(), config.into()));
            if let Err(error) = qq.authenticate().await {
                return Self::token_error_response(&error);
            }
            let mut channel_registration = self.channel_registration.lock().await;
            let previous_configuration =
                match self.storage.get_bytes(CONFIGURATION_STORAGE_KEY).await {
                    Ok(configuration) => configuration,
                    Err(error) => {
                        log::error!(
                            "failed to read the previous QQ gateway configuration: {error}"
                        );
                        return Self::response(500, br#"{"error":"storage"}"#);
                    }
                };
            if let Err(error) = self
                .storage
                .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
                .await
            {
                log::error!("failed to persist QQ gateway configuration: {error}");
                return Self::response(500, br#"{"error":"storage"}"#);
            }
            let channel: Rc<dyn MessageChannel> = qq;
            channel_registration.take();
            self.configured.set(false);
            match self.gateway.register(channel) {
                Ok(registration) => {
                    channel_registration.replace(registration);
                    self.configured.set(true);
                    log::info!("configured QQ gateway provider");
                    Self::response(204, b"")
                }
                Err(error) => {
                    let restored = if let Some(previous) = previous_configuration.as_deref() {
                        self.storage.put(CONFIGURATION_STORAGE_KEY, previous).await
                    } else {
                        self.storage.delete(CONFIGURATION_STORAGE_KEY).await
                    };
                    if let Err(storage_error) = restored {
                        log::error!(
                            "failed to roll back rejected QQ gateway configuration: {storage_error}"
                        );
                        return Self::response(500, br#"{"error":"storage"}"#);
                    }
                    log::warn!("rejected QQ gateway configuration: {error}");
                    Self::response(422, br#"{"error":"registration_failed"}"#)
                }
            }
        })
    }
}

fn encode_configuration(config: &ConfigRequest) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(config)
}

fn decode_configuration(bytes: &[u8]) -> Result<ConfigRequest, serde_json::Error> {
    serde_json::from_slice(bytes)
}

fn is_legacy_configuration(bytes: &[u8]) -> bool {
    serde_json::from_slice::<LegacyConfiguration>(bytes).is_ok()
}

async fn load_configuration<Storage: PluginStorage>(
    storage: &Storage,
) -> PluginResult<Option<ConfigRequest>> {
    let Some(bytes) = storage.get_bytes(CONFIGURATION_STORAGE_KEY).await? else {
        return Ok(None);
    };
    match decode_configuration(&bytes) {
        Ok(config) => Ok(Some(config)),
        Err(_) if is_legacy_configuration(&bytes) => {
            log::warn!(
                "ignoring stored QQ configuration that holds an access token instead of an App Secret; QQ stays unconfigured until it is saved again"
            );
            Ok(None)
        }
        Err(error) => Err(PluginError::registration(error)),
    }
}

/// Registers the stored QQ channel, if any. The access token is fetched lazily
/// on the first send, so restoring makes no network request.
async fn restore_channel<Storage, T, D>(
    storage: &Storage,
    gateway: &IMessageGateway,
    http_clients: &ClientFactory<'static, T, D>,
) -> PluginResult<Option<MessageChannelRegistration>>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    load_configuration(storage)
        .await?
        .map(|config| {
            let channel: Rc<dyn MessageChannel> =
                Rc::new(QQ::new(http_clients.clone(), config.into()));
            gateway.register(channel).map_err(PluginError::registration)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use barracuda_imessage_gateway_plugin::IMessageGatewayPlugin;
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, never_embassy_stack, ScriptStep, ScriptedStack,
    };
    use barracuda_plugin::manager::{PluginDeclaration, PluginManager};
    use barracuda_workflow_plugin::WorkflowPlugin;
    use futures_lite::future::block_on;

    use super::*;

    const TOKEN_URL: &str = "http://qq.test/app/getAppAccessToken";
    const REJECTED: &str = r#"{"code":10004,"message":"机器人不存在"}"#;

    #[derive(Clone, Copy)]
    enum Scenario {
        RejectedCredentials,
        UnreachableTokenEndpoint,
        AcceptedCredentials,
        LegacyRequestBody,
        LegacyStoredConfiguration,
    }

    /// Exercises one scenario against its own Plugin storage and the real Gateway.
    struct Probe {
        scenario: Scenario,
        network: &'static ScriptedStack,
        completed: Rc<Cell<bool>>,
    }

    impl PluginDeclaration for Probe {
        const ID: &'static str = "qq-probe";
        const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway"];
    }

    impl Plugin for Probe {
        fn register<Storage>(
            &mut self,
            context: &mut PluginRegisterContext<'_, Storage>,
        ) -> PluginResult<()>
        where
            Storage: PluginStorage,
        {
            let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
            let http_clients = ClientFactory::from_network(self.network, self.network);
            let storage = context.storage().clone();
            match self.scenario {
                Scenario::LegacyStoredConfiguration => {
                    block_on(storage.put(
                        CONFIGURATION_STORAGE_KEY,
                        br#"{"app_id":"app","access_token":"old-token","api_base":"https://api.sgroup.qq.com"}"#
                            .as_slice(),
                    ))?;
                    let restored = block_on(restore_channel(&storage, &gateway, &http_clients))?;
                    assert!(restored.is_none());
                    assert!(block_on(storage.get_bytes(CONFIGURATION_STORAGE_KEY))?.is_some());

                    let current = ConfigRequest {
                        app_id: "app".into(),
                        app_secret: "secret".into(),
                        api_base: default_qq_api_base(),
                        token_url: TOKEN_URL.into(),
                    };
                    let bytes =
                        encode_configuration(&current).map_err(PluginError::registration)?;
                    block_on(storage.put(CONFIGURATION_STORAGE_KEY, bytes.as_slice()))?;
                    let restored = block_on(restore_channel(&storage, &gateway, &http_clients))?;
                    assert!(restored.is_some());
                    drop(restored);

                    block_on(
                        storage.put(CONFIGURATION_STORAGE_KEY, br#"{"app_id":1}"#.as_slice()),
                    )?;
                    assert!(block_on(restore_channel(&storage, &gateway, &http_clients)).is_err());
                    assert!(self.network.requests().is_empty());
                }
                scenario => {
                    let configured = Rc::new(Cell::new(false));
                    let endpoint = ConfigEndpoint {
                        gateway,
                        http_clients,
                        channel_registration: Mutex::new(None),
                        configured: Rc::clone(&configured),
                        storage: storage.clone(),
                    };
                    let get = || {
                        let response = block_on(
                            endpoint.handle(HttpRequest::new(HttpMethod::Get, Vec::new())),
                        );
                        assert_eq!(response.status(), 200);
                        response.body().map(<[u8]>::to_vec)
                    };
                    assert_eq!(get().as_deref(), Some(&br#"{"configured":false}"#[..]));
                    let body: &[u8] = match scenario {
                        Scenario::LegacyRequestBody => {
                            br#"{"app_id":"app","access_token":"old-token"}"#
                        }
                        _ => {
                            br#"{"app_id":"app","app_secret":"secret","token_url":"http://qq.test/app/getAppAccessToken"}"#
                        }
                    };
                    let response = block_on(
                        endpoint.handle(HttpRequest::new(HttpMethod::Post, body.to_vec())),
                    );
                    let response_body = response.body().expect("buffered response");
                    let stored = block_on(storage.get_bytes(CONFIGURATION_STORAGE_KEY))?;
                    let registered = block_on(endpoint.channel_registration.lock()).is_some();
                    assert_eq!(configured.get(), registered);
                    let expected: &[u8] = if registered {
                        br#"{"configured":true}"#
                    } else {
                        br#"{"configured":false}"#
                    };
                    assert_eq!(get().as_deref(), Some(expected));
                    match scenario {
                        Scenario::RejectedCredentials => {
                            assert_eq!(response.status(), 422);
                            assert_eq!(
                                response_body,
                                r#"{"error":"verification_failed","message":"机器人不存在","code":"10004"}"#
                                    .as_bytes()
                            );
                            assert!(stored.is_none());
                            assert!(!registered);
                        }
                        Scenario::UnreachableTokenEndpoint => {
                            assert_eq!(response.status(), 502);
                            assert!(response_body
                                .starts_with(br#"{"error":"upstream_unavailable","message":"#));
                            assert!(stored.is_none());
                            assert!(!registered);
                        }
                        Scenario::AcceptedCredentials => {
                            assert_eq!(response.status(), 204);
                            assert!(response_body.is_empty());
                            let stored = stored.expect("configuration stored");
                            let config =
                                decode_configuration(&stored).map_err(PluginError::registration)?;
                            assert_eq!(config.app_secret, "secret");
                            assert_eq!(config.token_url, TOKEN_URL);
                            assert_eq!(config.api_base, "https://api.sgroup.qq.com");
                            assert!(!String::from_utf8_lossy(&stored).contains("token-1"));
                            assert!(registered);
                        }
                        Scenario::LegacyRequestBody => {
                            assert_eq!(response.status(), 400);
                            assert_eq!(response_body, br#"{"error":"invalid_request"}"#);
                            assert!(stored.is_none());
                            assert!(self.network.requests().is_empty());
                        }
                        Scenario::LegacyStoredConfiguration => {}
                    }
                    let request = self.network.requests().join("\n");
                    assert!(!String::from_utf8_lossy(response_body).contains("secret"));
                    assert!(!String::from_utf8_lossy(response_body).contains("token-1"));
                    if !request.is_empty() {
                        assert!(request.starts_with("POST /app/getAppAccessToken HTTP/1.1"));
                    }
                }
            }
            self.completed.set(true);
            Ok(())
        }
    }

    fn plugin_context() -> PluginContext {
        let stack = never_embassy_stack();
        let info = barracuda_plugin::api::TargetIdentity::new(
            barracuda_plugin::api::PlatformInfo::new("test", "test", "test-arch", "hosted"),
            barracuda_plugin::api::BoardInfo::new(
                "test-board",
                barracuda_plugin::api::Hardware::new("test-chip"),
            ),
        );
        PluginContext::new(info, stack, ClientFactory::plaintext(stack))
    }

    fn run(scenario: Scenario, steps: impl IntoIterator<Item = ScriptStep>) {
        block_on(install_global_memory_vfs()).expect("install test VFS");
        let partition = block_on(memory_partition(64 * 1024)).expect("create test database region");
        let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
        manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
        let mut context = plugin_context();
        let completed = Rc::new(Cell::new(false));
        manager
            .register(WorkflowPlugin::new(&mut context))
            .expect("register Workflow Plugin");
        manager
            .register(IMessageGatewayPlugin::new(&mut context))
            .expect("register IMessage Gateway Plugin");
        manager
            .register(Probe {
                scenario,
                network: Box::leak(Box::new(ScriptedStack::new(steps))),
                completed: Rc::clone(&completed),
            })
            .expect("exercise QQ configuration");
        assert!(completed.get());
    }

    #[test]
    fn stored_configuration_round_trips_every_field() -> Result<(), serde_json::Error> {
        let config = ConfigRequest {
            app_id: "app".into(),
            app_secret: "secret".into(),
            api_base: "https://qq.example".into(),
            token_url: "https://qq.example/token".into(),
        };

        let bytes = encode_configuration(&config)?;
        let restored = decode_configuration(&bytes)?;

        assert_eq!(restored.app_id, config.app_id);
        assert_eq!(restored.app_secret, config.app_secret);
        assert_eq!(restored.api_base, config.api_base);
        assert_eq!(restored.token_url, config.token_url);
        Ok(())
    }

    #[test]
    fn request_defaults_api_base_and_token_url() -> Result<(), serde_json::Error> {
        let config: ConfigRequest =
            serde_json::from_slice(br#"{"app_id":"app","app_secret":"secret"}"#)?;
        assert_eq!(config.api_base, "https://api.sgroup.qq.com");
        assert_eq!(
            config.token_url,
            "https://bots.qq.com/app/getAppAccessToken"
        );
        Ok(())
    }

    #[test]
    fn rejected_credentials_return_verification_failed_and_store_nothing() {
        run(
            Scenario::RejectedCredentials,
            [ScriptStep::json(200, REJECTED)],
        );
    }

    #[test]
    fn unreachable_token_endpoint_returns_upstream_unavailable() {
        run(
            Scenario::UnreachableTokenEndpoint,
            [ScriptStep::response(
                502,
                "text/html",
                b"<html>bad gateway</html>",
                usize::MAX,
            )],
        );
    }

    #[test]
    fn accepted_credentials_store_the_app_secret_but_not_the_token() {
        run(
            Scenario::AcceptedCredentials,
            [ScriptStep::json(
                200,
                r#"{"access_token":"token-1","expires_in":"7200"}"#,
            )],
        );
    }

    #[test]
    fn access_token_request_body_is_invalid() {
        run(Scenario::LegacyRequestBody, []);
    }

    #[test]
    fn legacy_stored_configuration_leaves_qq_unconfigured() {
        run(Scenario::LegacyStoredConfiguration, []);
    }
}

//! IMessage Wechat provider Plugin.

#![no_std]

extern crate alloc;

mod login;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use barracuda_captive_portal_plugin::{
    CaptivePortal, EntryState, EntryStatus, ResourceFiles, WebEntry, WebGroup, WebText,
};
use barracuda_imessage_gateway_plugin::IMessageGateway;
use barracuda_imessage_gateway_plugin::{MessageChannel, MessageChannelRegistration};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginStorage,
    PluginTaskToken,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use embassy_futures::select::select;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use http_client::ClientFactory;
use login::{LoginEndpoint, LoginRuntime, LoginTiming, LoginWatch};
use serde::{Deserialize, Serialize};
use wechat::{Wechat, WechatConfig};

/// HTTP path accepting Wechat configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/wechat";

/// HTTP path running the WeChat QR login session.
pub const LOGIN_API_PATH: &str = "/api/gateway/wechat/login";

const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Plugin that exposes Wechat configuration and registers the resulting channel.
#[barracuda_plugin::macros::plugin]
pub struct IMessageWechatPlugin {
    http_clients: ClientFactory<'static>,
    login: Option<LoginRuntime>,
}

impl IMessageWechatPlugin {
    /// Creates an unconfigured provider using Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
            login: None,
        }
    }
}

impl Plugin for IMessageWechatPlugin {
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
        let gateway = context.require::<IMessageGateway>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let webserver = context.require::<WebServer>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[1],
        )?;
        let channel_registration =
            embassy_futures::block_on(load_configuration(context.storage()))?
                .map(|config| {
                    let channel: Rc<dyn MessageChannel> =
                        Rc::new(Wechat::new(self.http_clients.clone(), config.into()));
                    gateway.register(channel).map_err(PluginError::registration)
                })
                .transpose()?;
        let configuration = Rc::new(ChannelConfiguration {
            gateway,
            http_clients: self.http_clients.clone(),
            configured: Cell::new(channel_registration.is_some()),
            channel_registration: Mutex::new(channel_registration),
            storage: context.storage().clone(),
        });
        let (login_endpoint, login_runtime) = LoginEndpoint::new(
            Rc::clone(&configuration),
            default_wechat_api_base(),
            LoginTiming::DEVICE,
        );
        let status_configuration = Rc::clone(&configuration);
        let login_watch = login_endpoint.watch();
        let portal = context.require::<CaptivePortal>("captive-portal")?;
        context.retain(
            portal
                .register_with_status(
                    WebEntry {
                        id: "imessage-wechat",
                        group: WebGroup::Channel,
                        order: 30,
                        title: WebText {
                            zh: "微信",
                            en: "WeChat",
                        },
                        summary: WebText {
                            zh: "微信消息通道",
                            en: "WeChat message channel",
                        },
                        icon: Some("icon.svg"),
                        figure: None,
                        module: "entry.js",
                    },
                    ResourceFiles::from(context.filesystem()?.clone()),
                    move || entry_status(&status_configuration, &login_watch),
                )
                .map_err(PluginError::registration)?,
        );
        context.retain(
            webserver
                .serve_http(CONFIG_API_PATH, ConfigEndpoint { configuration })
                .map_err(PluginError::registration)?,
        );
        context.retain(
            webserver
                .serve_http(LOGIN_API_PATH, login_endpoint)
                .map_err(PluginError::registration)?,
        );
        self.login = Some(login_runtime);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .login
            .take()
            .ok_or_else(|| PluginError::registration(LoginRuntimeUnavailable))?;
        let task =
            wechat_login_task(runtime, context.task_token()).map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

/// `attention` while a QR login waits for its scan, otherwise whether a channel
/// is registered.
fn entry_status<Storage, T, D>(
    configuration: &ChannelConfiguration<Storage, T, D>,
    login: &LoginWatch,
) -> EntryStatus {
    if login.waiting_for_scan() {
        EntryStatus::new(
            EntryState::Attention,
            WebText {
                zh: "等待扫码",
                en: "Waiting for scan",
            },
        )
    } else {
        EntryStatus::configured(configuration.is_configured())
    }
}

/// Owns the QR login session; parked on its command signal while no session runs.
#[embassy_executor::task]
async fn wechat_login_task(runtime: LoginRuntime, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime).await;
    log::info!("stopped WeChat login task");
}

#[derive(Debug, thiserror::Error)]
#[error("WeChat login runtime was not prepared during Plugin registration")]
struct LoginRuntimeUnavailable;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
    wechat::DEFAULT_API_BASE.into()
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

impl ConfigRequest {
    /// Builds the configuration stored for a bot token obtained by QR login.
    fn from_login(token: String, api_base: String) -> Self {
        Self {
            token,
            api_base,
            app_id: default_app_id(),
            client_version: default_client_version(),
            route_tag: None,
            x_wechat_uin: default_x_wechat_uin(),
        }
    }
}

/// Stored WeChat configuration and the channel registered from it.
///
/// Shared by the manual configuration endpoint and the QR login runtime so both
/// persist and register through one path.
struct ChannelConfiguration<
    Storage,
    T: 'static = http_client::Tcp,
    D: 'static = http_client::Resolver,
> {
    gateway: Rc<IMessageGateway>,
    http_clients: ClientFactory<'static, T, D>,
    channel_registration: Mutex<NoopRawMutex, Option<MessageChannelRegistration>>,
    /// Whether a channel is registered with the Gateway, readable without the lock.
    configured: Cell<bool>,
    storage: Storage,
}

impl<Storage, T, D> ChannelConfiguration<Storage, T, D> {
    /// Returns whether this Plugin currently has a WeChat channel registered.
    fn is_configured(&self) -> bool {
        self.configured.get()
    }
}

/// Why a configuration was not applied.
enum ConfigureError {
    /// Reading, writing, or rolling back storage failed.
    Storage,
    /// The Gateway rejected the channel; the previous configuration was restored.
    Registration,
}

impl<Storage, T, D> ChannelConfiguration<Storage, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    /// Persists `config`, then replaces the registered channel with one built from it.
    async fn apply(&self, config: ConfigRequest) -> Result<(), ConfigureError> {
        let mut channel_registration = self.channel_registration.lock().await;
        let previous_configuration = match self.storage.get_bytes(CONFIGURATION_STORAGE_KEY).await {
            Ok(configuration) => configuration,
            Err(error) => {
                log::error!("failed to read the previous Wechat gateway configuration: {error}");
                return Err(ConfigureError::Storage);
            }
        };
        let Ok(bytes) = encode_configuration(&config) else {
            log::error!("failed to encode Wechat gateway configuration");
            return Err(ConfigureError::Storage);
        };
        if let Err(error) = self
            .storage
            .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
            .await
        {
            log::error!("failed to persist Wechat gateway configuration: {error}");
            return Err(ConfigureError::Storage);
        }
        let channel: Rc<dyn MessageChannel> =
            Rc::new(Wechat::new(self.http_clients.clone(), config.into()));
        channel_registration.take();
        self.configured.set(false);
        match self.gateway.register(channel) {
            Ok(registration) => {
                channel_registration.replace(registration);
                self.configured.set(true);
                log::info!("configured Wechat gateway provider");
                Ok(())
            }
            Err(error) => {
                let restored = if let Some(previous) = previous_configuration.as_deref() {
                    self.storage.put(CONFIGURATION_STORAGE_KEY, previous).await
                } else {
                    self.storage.delete(CONFIGURATION_STORAGE_KEY).await
                };
                if let Err(storage_error) = restored {
                    log::error!(
                        "failed to roll back rejected Wechat gateway configuration: {storage_error}"
                    );
                    return Err(ConfigureError::Storage);
                }
                log::warn!("rejected Wechat gateway configuration: {error}");
                Err(ConfigureError::Registration)
            }
        }
    }

    /// Returns the API base of the stored configuration, if one is stored.
    async fn stored_api_base(&self) -> Option<String> {
        match load_configuration(&self.storage).await {
            Ok(config) => config.map(|config| config.api_base),
            Err(error) => {
                log::warn!("failed to read the stored Wechat gateway configuration: {error}");
                None
            }
        }
    }
}

struct ConfigEndpoint<Storage, T: 'static = http_client::Tcp, D: 'static = http_client::Resolver> {
    configuration: Rc<ChannelConfiguration<Storage, T, D>>,
}

fn json_response(status: u16, body: &'static [u8]) -> HttpResponse {
    HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
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
                HttpMethod::Get if self.configuration.is_configured() => {
                    return json_response(200, br#"{"configured":true}"#);
                }
                HttpMethod::Get => return json_response(200, br#"{"configured":false}"#),
                _ => return json_response(405, br#"{"error":"method_not_allowed"}"#),
            }
            let Ok(config) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                log::warn!("rejected invalid Wechat gateway configuration");
                return json_response(400, br#"{"error":"invalid_request"}"#);
            };
            match self.configuration.apply(config).await {
                Ok(()) => json_response(204, b""),
                Err(ConfigureError::Storage) => json_response(500, br#"{"error":"storage"}"#),
                Err(ConfigureError::Registration) => {
                    json_response(422, br#"{"error":"registration_failed"}"#)
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

async fn load_configuration<Storage: PluginStorage>(
    storage: &Storage,
) -> PluginResult<Option<ConfigRequest>> {
    storage
        .get_bytes(CONFIGURATION_STORAGE_KEY)
        .await?
        .map(|bytes| decode_configuration(&bytes))
        .transpose()
        .map_err(PluginError::registration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_configuration_round_trips_every_field() -> Result<(), serde_json::Error> {
        let config = ConfigRequest {
            token: "secret".into(),
            api_base: "https://wechat.example".into(),
            app_id: "bot-app".into(),
            client_version: "42".into(),
            route_tag: Some("route".into()),
            x_wechat_uin: "MQ==".into(),
        };

        let bytes = encode_configuration(&config)?;
        let restored = decode_configuration(&bytes)?;

        assert_eq!(restored.token, config.token);
        assert_eq!(restored.api_base, config.api_base);
        assert_eq!(restored.app_id, config.app_id);
        assert_eq!(restored.client_version, config.client_version);
        assert_eq!(restored.route_tag, config.route_tag);
        assert_eq!(restored.x_wechat_uin, config.x_wechat_uin);
        Ok(())
    }
}

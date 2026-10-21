//! IMessage Wechat provider Plugin.

#![no_std]
#![recursion_limit = "256"]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(test)]
mod bench;
mod login;
mod media;
mod receive;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use barracuda_captive_portal_plugin::{
    CaptivePortal, EntryState, EntryStatus, ResourceFiles, WebEntry, WebGroup, WebText,
};
use barracuda_imessage_gateway_plugin::{
    ChannelControl, ChannelLevel, ChannelMode, IMessageGateway, MessageChannel,
    MessageChannelRegistration, ModeError, ModeFuture, OnDemand, Owners, OwnersError,
    ReceiveControl, ReceiveRuntime, ReceiveSlots, ReceiveTiming,
};
use barracuda_plugin::api::{PluginContext, SharedEntropy};
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginStorage,
    PluginTaskToken,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebRouteRegistration,
    WebServer, WebServerError,
};
use embassy_futures::select::select;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use login::{LoginEndpoint, LoginRuntime, LoginTiming, LoginWatch};
use receive::{InboundState, PollTiming, SESSION_EXPIRED_CODE};
use serde::{Deserialize, Serialize};
use wechat::{Wechat, WechatConfig};

/// HTTP path accepting Wechat configuration; the Gateway serves the
/// channel's `/status`, `/mode`, and `/owners` below it.
pub const CONFIG_API_PATH: &str = "/api/gateway/wechat";

/// HTTP path running the WeChat QR login session.
pub const LOGIN_API_PATH: &str = "/api/gateway/wechat/login";

/// Gateway channel name.
const CHANNEL: &str = "wechat";

const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Plugin that exposes Wechat configuration and registers the resulting channel.
#[barracuda_plugin::macros::plugin]
pub struct IMessageWechatPlugin {
    entropy: SharedEntropy,
    login: Option<LoginRuntime>,
    receive: Option<ReceiveRuntime>,
}

impl IMessageWechatPlugin {
    /// Creates the unregistered WeChat channel Plugin; the Platform entropy
    /// randomizes each iLink request's `X-WECHAT-UIN`.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            entropy: context.entropy.clone(),
            login: None,
            receive: None,
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
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let webserver = context.require::<WebServer>("webserver")?;
        let portal = context.require::<CaptivePortal>("captive-portal")?;
        let built = build_channel(
            context.storage().clone(),
            Rc::clone(&gateway),
            self.entropy.clone(),
            default_wechat_api_base(),
            Timing::DEVICE,
        )?;
        let status_channel = Rc::clone(&built.channel);
        let login_watch = built.login.watch();
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
                    move || entry_status(&status_channel, &login_watch),
                )
                .map_err(PluginError::registration)?,
        );
        for route in serve_routes(&gateway, &webserver, &built.channel, built.login)
            .map_err(PluginError::registration)?
        {
            context.retain(route);
        }
        self.login = Some(built.login_runtime);
        self.receive = Some(built.receive_runtime);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let login = self
            .login
            .take()
            .ok_or_else(|| PluginError::registration(RuntimeUnavailable))?;
        let receive = self
            .receive
            .take()
            .ok_or_else(|| PluginError::registration(RuntimeUnavailable))?;
        let login =
            wechat_login_task(login, context.task_token()).map_err(PluginError::registration)?;
        let receive = wechat_receive_task(receive, context.task_token())
            .map_err(PluginError::registration)?;
        let spawner = context.task_spawner()?;
        spawner.spawn(login);
        spawner.spawn(receive);
        Ok(())
    }
}

/// Time limits of the channel's runtimes.
#[derive(Clone, Copy)]
struct Timing {
    poll: PollTiming,
    receive: ReceiveTiming,
    login: LoginTiming,
}

impl Timing {
    /// Limits used on the device.
    const DEVICE: Self = Self {
        poll: PollTiming::DEVICE,
        receive: ReceiveTiming::DEVICE,
        login: LoginTiming::DEVICE,
    };
}

/// The channel, its login endpoint, and the runtimes its Plugin's tasks own.
struct BuiltChannel<Storage> {
    channel: Rc<ChannelConfiguration<Storage>>,
    login: LoginEndpoint<Storage>,
    login_runtime: LoginRuntime,
    receive_runtime: ReceiveRuntime,
}

/// Loads the channel from storage, registers it with the Gateway when it is
/// linked and its mode registers, and builds its login endpoint and the
/// runtimes its Plugin's tasks own. A QR login uses `login_api_base` while no
/// configuration is stored.
fn build_channel<Storage: PluginStorage>(
    storage: Storage,
    gateway: Rc<IMessageGateway>,
    entropy: SharedEntropy,
    login_api_base: String,
    timing: Timing,
) -> PluginResult<BuiltChannel<Storage>> {
    let channel = Rc::new(embassy_futures::block_on(ChannelConfiguration::load(
        gateway,
        storage,
        entropy,
        timing.poll,
    ))?);
    let receive_runtime = channel.gateway.receive_runtime(
        Rc::clone(&channel.receive),
        Rc::clone(&channel),
        timing.receive,
    );
    // Without a free slot the control reports `no_slot` and retries.
    let _no_slot = channel.gateway.sync_receive(&*channel);
    let (login, login_runtime) =
        LoginEndpoint::new(Rc::clone(&channel), login_api_base, timing.login);
    Ok(BuiltChannel {
        channel,
        login,
        login_runtime,
        receive_runtime,
    })
}

/// Serves the channel's routes: the configuration path, with the Gateway's
/// shared `/status`, `/mode`, and `/owners` below it, and the exact login
/// route, which takes precedence over that prefix.
fn serve_routes<Storage: PluginStorage>(
    gateway: &IMessageGateway,
    webserver: &WebServer,
    channel: &Rc<ChannelConfiguration<Storage>>,
    login: LoginEndpoint<Storage>,
) -> Result<[WebRouteRegistration; 2], WebServerError> {
    let config = ConfigEndpoint {
        configuration: Rc::clone(channel),
    };
    Ok([
        gateway.serve_channel(CONFIG_API_PATH, Rc::clone(channel), config)?,
        webserver.serve_http(LOGIN_API_PATH, login)?,
    ])
}

/// Portal status: `attention` while a QR login waits for its scan or the bot
/// session expired, otherwise the Gateway's channel status.
fn entry_status<Storage: PluginStorage>(
    configuration: &ChannelConfiguration<Storage>,
    login: &LoginWatch,
) -> EntryStatus {
    if login.waiting_for_scan() {
        return EntryStatus::new(
            EntryState::Attention,
            WebText {
                zh: "等待扫码",
                en: "Waiting for scan",
            },
        );
    }
    let session_expired = configuration.mode().receives()
        && configuration.receive.state().code() == Some(SESSION_EXPIRED_CODE);
    if configuration.configured() && session_expired {
        return EntryStatus::new(
            EntryState::Attention,
            WebText {
                zh: "需要重新扫码",
                en: "Scan again to relink",
            },
        );
    }
    let status = configuration.gateway.channel_status(configuration);
    let state = match status.level {
        ChannelLevel::Off => EntryState::Off,
        ChannelLevel::Ready => EntryState::Ready,
        ChannelLevel::Attention => EntryState::Attention,
    };
    EntryStatus::new(
        state,
        WebText {
            zh: status.zh,
            en: status.en,
        },
    )
}

/// Owns the QR login session; parked on its command signal while no session runs.
#[embassy_executor::task]
async fn wechat_login_task(runtime: LoginRuntime, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime).await;
    log::info!("stopped WeChat login task");
}

/// Owns the `getupdates` long poll; parked while the channel does not receive.
#[embassy_executor::task]
async fn wechat_receive_task(runtime: ReceiveRuntime, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime).await;
    log::info!("stopped WeChat receive task");
}

#[derive(Debug, thiserror::Error)]
#[error("WeChat runtimes were not prepared during Plugin registration")]
struct RuntimeUnavailable;

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
    #[serde(default = "default_cdn_base")]
    cdn_base: String,
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
            cdn_base: value.cdn_base,
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
fn default_cdn_base() -> String {
    wechat::DEFAULT_CDN_BASE.into()
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
            cdn_base: default_cdn_base(),
        }
    }
}

/// The WeChat channel: stored configuration, mode, Gateway registration,
/// allowed accounts, and receive state.
///
/// Shared by the configuration, mode, owners, and login endpoints, the QR
/// login runtime, and the receive runtime, so all of them persist and
/// register through one path.
struct ChannelConfiguration<Storage> {
    gateway: Rc<IMessageGateway>,
    /// Serializes configuration and mode changes; holds the registration that
    /// exists exactly while a provider is configured and the mode registers.
    channel_registration: Mutex<NoopRawMutex, Option<MessageChannelRegistration>>,
    /// The provider built from the stored configuration.
    provider: RefCell<Option<Rc<Wechat>>>,
    mode: Cell<ChannelMode>,
    /// Loaded once a bot is linked.
    owners: OnDemand<Owners<Storage>>,
    receive: Rc<ReceiveControl<ReceiveSlots>>,
    inbound: InboundState,
    entropy: SharedEntropy,
    storage: Storage,
}

/// Why a configuration was not applied.
enum ConfigureError {
    /// Reading, writing, or rolling back storage failed.
    Storage,
    /// The Gateway rejected the channel; the previous configuration was restored.
    Registration,
}

impl<Storage: PluginStorage> ChannelConfiguration<Storage> {
    /// Loads the stored configuration, mode, owners, and receive state, and
    /// registers the channel when it is configured and its mode registers.
    async fn load(
        gateway: Rc<IMessageGateway>,
        storage: Storage,
        entropy: SharedEntropy,
        timing: PollTiming,
    ) -> PluginResult<Self> {
        let config = load_configuration(&storage).await?;
        // WeChat has no send-only mode: iLink sends need the inbound
        // `context_token`, so configurations stored before modes receive.
        let mode = match gateway
            .load_mode(&storage, config.is_some(), ChannelMode::SendReceive)
            .await?
        {
            ChannelMode::Send => ChannelMode::SendReceive,
            mode => mode,
        };
        let inbound = InboundState::load(&storage, timing).await;
        let provider = config.map(|config| {
            Rc::new(
                Wechat::with_context_tokens(
                    gateway.http(),
                    config.into(),
                    inbound.context_tokens(),
                )
                .with_entropy(entropy.clone()),
            )
        });
        let registration = match &provider {
            Some(provider) if mode.registers() => Some(
                gateway
                    .register(Rc::clone(provider) as Rc<dyn MessageChannel>)
                    .map_err(PluginError::registration)?,
            ),
            _ => None,
        };
        let receive = Rc::new(gateway.receive_control());
        let configuration = Self {
            gateway,
            channel_registration: Mutex::new(registration),
            provider: RefCell::new(provider),
            mode: Cell::new(mode),
            owners: OnDemand::new(),
            receive,
            inbound,
            entropy,
            storage,
        };
        if configuration.is_configured() {
            configuration
                .load_owners()
                .await
                .map_err(PluginError::registration)?;
        }
        Ok(configuration)
    }

    /// Loads the owner book, once; a linked channel needs it.
    async fn load_owners(&self) -> Result<&Owners<Storage>, OwnersError> {
        self.owners
            .get_or_load(|| self.gateway.load_owners(self.storage.clone()))
            .await
    }

    /// Returns whether a WeChat bot is linked (a configuration is stored).
    fn is_configured(&self) -> bool {
        self.provider.borrow().is_some()
    }

    /// The provider of the stored configuration.
    fn provider(&self) -> Option<Rc<Wechat>> {
        self.provider.borrow().clone()
    }

    /// Applies `config`, then (re)starts receiving with it.
    async fn configure(&self, config: ConfigRequest) -> Result<(), ConfigureError> {
        self.apply(config).await?;
        self.restart_receive();
        Ok(())
    }

    /// Applies the configuration of a confirmed QR login for bot `bot_id`.
    ///
    /// When the bot differs from the last linked one, the stored cursor and
    /// context tokens belong to the old bot and are cleared. Receiving
    /// restarts, which also resumes a loop halted by an expired session.
    async fn link(
        &self,
        config: ConfigRequest,
        bot_id: Option<&str>,
    ) -> Result<(), ConfigureError> {
        self.apply(config).await?;
        self.inbound.relink(&self.storage, bot_id).await;
        self.restart_receive();
        Ok(())
    }

    fn restart_receive(&self) {
        // Without a free slot the control reports `no_slot` and retries.
        let _no_slot = self.gateway.sync_receive(self);
        self.receive.restart();
    }

    /// Persists `config`, then replaces the provider and, while the mode
    /// registers, the Gateway registration.
    async fn apply(&self, config: ConfigRequest) -> Result<(), ConfigureError> {
        let mut channel_registration = self.channel_registration.lock().await;
        if let Err(error) = self.load_owners().await {
            log::error!("failed to read the Wechat owners: {error}");
            return Err(ConfigureError::Storage);
        }
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
        let provider = Rc::new(
            Wechat::with_context_tokens(
                self.gateway.http(),
                config.into(),
                self.inbound.context_tokens(),
            )
            .with_entropy(self.entropy.clone()),
        );
        let previous_provider = self.provider.replace(Some(Rc::clone(&provider)));
        if !self.mode.get().registers() {
            log::info!("configured Wechat gateway provider (disabled)");
            return Ok(());
        }
        channel_registration.take();
        let error = match self.gateway.register(provider) {
            Ok(registration) => {
                channel_registration.replace(registration);
                log::info!("configured Wechat gateway provider");
                return Ok(());
            }
            Err(error) => error,
        };
        let restored = if let Some(previous) = previous_configuration.as_deref() {
            self.storage.put(CONFIGURATION_STORAGE_KEY, previous).await
        } else {
            self.storage.delete(CONFIGURATION_STORAGE_KEY).await
        };
        if let Some(previous) = &previous_provider {
            if let Ok(registration) = self
                .gateway
                .register(Rc::clone(previous) as Rc<dyn MessageChannel>)
            {
                channel_registration.replace(registration);
            }
        }
        self.provider.replace(previous_provider);
        if let Err(storage_error) = restored {
            log::error!(
                "failed to roll back rejected Wechat gateway configuration: {storage_error}"
            );
            return Err(ConfigureError::Storage);
        }
        log::warn!("rejected Wechat gateway configuration: {error}");
        Err(ConfigureError::Registration)
    }

    /// Stores `mode` and adds or drops the Gateway registration to match.
    async fn set_mode(&self, mode: ChannelMode) -> Result<(), ModeError> {
        let mut channel_registration = self.channel_registration.lock().await;
        let previous = self.mode.get();
        if let Err(error) = self.gateway.store_mode(&self.storage, mode).await {
            log::error!("failed to store the WeChat channel mode: {error}");
            return Err(ModeError::Storage);
        }
        if !mode.registers() {
            channel_registration.take();
        } else if channel_registration.is_none() {
            if let Some(provider) = self.provider() {
                match self.gateway.register(provider) {
                    Ok(registration) => {
                        channel_registration.replace(registration);
                    }
                    Err(error) => {
                        log::warn!("rejected the WeChat channel: {error}");
                        if let Err(error) = self.gateway.store_mode(&self.storage, previous).await {
                            log::error!("failed to restore the WeChat channel mode: {error}");
                        }
                        return Err(ModeError::Registration);
                    }
                }
            }
        }
        self.mode.set(mode);
        Ok(())
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

impl<Storage: PluginStorage> ChannelControl for ChannelConfiguration<Storage> {
    type Storage = Storage;
    type Slots = ReceiveSlots;

    fn configured(&self) -> bool {
        self.is_configured()
    }

    fn mode(&self) -> ChannelMode {
        self.mode.get()
    }

    fn modes(&self) -> &'static [ChannelMode] {
        ChannelMode::WITHOUT_SEND_ONLY
    }

    fn apply_mode(&self, mode: ChannelMode) -> ModeFuture<'_> {
        Box::pin(self.set_mode(mode))
    }

    fn receive(&self) -> &ReceiveControl<Self::Slots> {
        &self.receive
    }

    fn owners(&self) -> Option<&Owners<Self::Storage>> {
        self.owners.get()
    }
}

/// `POST` stores a configuration; the channel status is under `/status`.
struct ConfigEndpoint<Storage> {
    configuration: Rc<ChannelConfiguration<Storage>>,
}

fn json_response(status: u16, body: &'static [u8]) -> HttpResponse {
    HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
}

impl<Storage: PluginStorage> HttpEndpoint for ConfigEndpoint<Storage> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Post => {}
                _ => return json_response(405, br#"{"error":"method_not_allowed"}"#),
            }
            let Ok(config) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                log::warn!("rejected invalid Wechat gateway configuration");
                return json_response(400, br#"{"error":"invalid_request"}"#);
            };
            match self.configuration.configure(config).await {
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
            cdn_base: "https://cdn.example/c2c".into(),
        };

        let bytes = encode_configuration(&config)?;
        let restored = decode_configuration(&bytes)?;

        assert_eq!(restored.token, config.token);
        assert_eq!(restored.api_base, config.api_base);
        assert_eq!(restored.app_id, config.app_id);
        assert_eq!(restored.client_version, config.client_version);
        assert_eq!(restored.route_tag, config.route_tag);
        assert_eq!(restored.x_wechat_uin, config.x_wechat_uin);
        assert_eq!(restored.cdn_base, config.cdn_base);
        let older = decode_configuration(br#"{"token":"t"}"#)?;
        assert_eq!(older.cdn_base, wechat::DEFAULT_CDN_BASE);
        Ok(())
    }
}

//! IMessage QQ provider Plugin.
#![no_std]
#![recursion_limit = "256"]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod channel;
mod login;
mod receive;
#[cfg(test)]
mod tests;

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use barracuda_captive_portal_plugin::{
    CaptivePortal, EntryState, EntryStatus, ResourceFiles, WebEntry, WebGroup, WebText,
};
use barracuda_imessage_gateway_plugin::{
    ChannelLevel, ChannelStatus, IMessageGateway, ReceiveRuntime, ReceiveTiming,
};
use barracuda_plugin::api::{PluginContext, SharedEntropy};
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginStorage,
    PluginTaskToken,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use embassy_futures::select::select;
use qq::{QQConfig, TokenError, QQ};
use serde::{de::IgnoredAny, Deserialize, Serialize};

use crate::channel::{build_channel, ConfigureError, QQChannel};
use crate::login::{LoginEndpoint, LoginRuntime, LoginTiming, LoginWatch};

/// HTTP path accepting QQ configuration; the Gateway serves the channel's
/// `/status`, `/mode`, and `/owners` below it.
pub const CONFIG_API_PATH: &str = "/api/gateway/qq";
/// HTTP path starting, observing, and cancelling a QR-code binding.
pub const LOGIN_API_PATH: &str = "/api/gateway/qq/login";

/// Gateway channel name.
const CHANNEL: &str = "qq";
const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Plugin that exposes QQ configuration, registers the resulting channel, and
/// receives its messages over the QQ WebSocket gateway.
#[barracuda_plugin::macros::plugin]
pub struct IMessageQQPlugin {
    entropy: SharedEntropy,
    runtime: Option<ReceiveRuntime>,
    login: Option<LoginRuntime>,
}

impl IMessageQQPlugin {
    /// Creates the unregistered QQ channel Plugin; the Platform entropy makes
    /// its WebSocket keys and masks and its scan-to-bind keys.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            entropy: context.entropy.clone(),
            runtime: None,
            login: None,
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
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let webserver = context.require::<WebServer>("webserver")?;
        let (channel, runtime) = build_channel(
            context.storage().clone(),
            Rc::clone(&gateway),
            self.entropy.clone(),
            ReceiveTiming::DEVICE,
        )?;
        let (login_endpoint, login_runtime) = LoginEndpoint::new(
            Rc::clone(&channel),
            qq::bind::DEFAULT_PORTAL_BASE.into(),
            LoginTiming::DEVICE,
        );
        let login_watch = login_endpoint.watch();
        let status = Rc::clone(&channel);
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
                    move || card_status(&status, &login_watch),
                )
                .map_err(PluginError::registration)?,
        );
        // The Gateway serves the configuration path and its `/status`,
        // `/mode`, and `/owners`; the exact login route takes its own
        // requests.
        let config = ConfigEndpoint {
            channel: Rc::clone(&channel),
        };
        context.retain(
            gateway
                .serve_channel(CONFIG_API_PATH, channel, config)
                .map_err(PluginError::registration)?,
        );
        context.retain(
            webserver
                .serve_http(LOGIN_API_PATH, login_endpoint)
                .map_err(PluginError::registration)?,
        );
        self.runtime = Some(runtime);
        self.login = Some(login_runtime);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(RuntimeUnavailable))?;
        let login = self
            .login
            .take()
            .ok_or_else(|| PluginError::registration(RuntimeUnavailable))?;
        let receive =
            qq_receive_task(runtime, context.task_token()).map_err(PluginError::registration)?;
        let login =
            qq_login_task(login, context.task_token()).map_err(PluginError::registration)?;
        let spawner = context.task_spawner()?;
        spawner.spawn(receive);
        spawner.spawn(login);
        Ok(())
    }
}

/// Portal status: `attention` while a QR binding waits for its scan,
/// otherwise the Gateway's status of the channel.
fn card_status<Storage: PluginStorage>(
    channel: &QQChannel<Storage>,
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
    entry_status(channel.gateway.channel_status(channel))
}

/// The portal card status of a channel status.
fn entry_status(status: ChannelStatus) -> EntryStatus {
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

/// Owns the QR binding session; parked on its command signal while no
/// session runs.
#[embassy_executor::task]
async fn qq_login_task(runtime: LoginRuntime, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime).await;
    log::info!("stopped QQ login task");
}

/// Owns the receive loop; parked without a slot unless the channel is
/// configured and in `send_receive`.
#[embassy_executor::task]
async fn qq_receive_task(runtime: ReceiveRuntime, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime).await;
    log::info!("stopped QQ receive task");
}

#[derive(Debug)]
struct RuntimeUnavailable;

impl core::fmt::Display for RuntimeUnavailable {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("QQ runtimes were not prepared during Plugin registration")
    }
}

impl core::error::Error for RuntimeUnavailable {}

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

/// `POST` verifies and stores a configuration; the channel status is under
/// `/status`.
struct ConfigEndpoint<Storage> {
    channel: Rc<QQChannel<Storage>>,
}

fn response(status: u16, body: &'static [u8]) -> HttpResponse {
    HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
}

fn error_response(status: u16, body: &ErrorBody<'_>) -> HttpResponse {
    match serde_json::to_vec(body) {
        Ok(bytes) => HttpResponse::new(status, JSON_CONTENT_TYPE, bytes),
        Err(_) => response(500, br#"{"error":"internal"}"#),
    }
}

fn token_error_response(error: &TokenError) -> HttpResponse {
    match error {
        TokenError::Rejected { code, message } => {
            log::warn!("QQ rejected the gateway credentials: {error}");
            error_response(
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
            error_response(
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

impl<Storage: PluginStorage> HttpEndpoint for ConfigEndpoint<Storage> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Post => {}
                _ => return response(405, br#"{"error":"method_not_allowed"}"#),
            }
            let Ok(config) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                log::warn!("rejected invalid QQ gateway configuration");
                return response(400, br#"{"error":"invalid_request"}"#);
            };
            let sender = Rc::new(QQ::new(self.channel.gateway.http(), config.clone().into()));
            if let Err(error) = sender.authenticate().await {
                return token_error_response(&error);
            }
            match self.channel.configure(config, sender).await {
                Ok(()) => response(204, b""),
                Err(ConfigureError::Storage) => response(500, br#"{"error":"storage"}"#),
                Err(ConfigureError::Registration) => {
                    response(422, br#"{"error":"registration_failed"}"#)
                }
            }
        })
    }
}

fn decode_configuration(bytes: &[u8]) -> Result<ConfigRequest, serde_json::Error> {
    serde_json::from_slice(bytes)
}

fn is_legacy_configuration(bytes: &[u8]) -> bool {
    serde_json::from_slice::<LegacyConfiguration>(bytes).is_ok()
}

/// Reads the stored configuration. One in the former `access_token` shape
/// leaves QQ unconfigured; other malformed data fails registration.
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

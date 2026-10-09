//! WeChat QR login: the HTTP endpoint and the Plugin-owned long-poll runtime.
//!
//! At most one login session exists. The browser starts, observes, and cancels
//! it through [`LoginEndpoint`]; the runtime future, spawned once as the
//! Plugin's login task, performs every iLink call. While no session runs, the
//! runtime is parked on its command signal and holds no upstream connection.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;

use barracuda_plugin::manager::PluginStorage;
use barracuda_webserver_plugin::{HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse};
use embassy_futures::select::{select, Either};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use serde::{Deserialize, Serialize};
use wechat::{LoginCredentials, LoginError, LoginStatus, WechatLogin};

use crate::{
    json_response, ChannelConfiguration, ConfigRequest, ConfigureError, JSON_CONTENT_TYPE,
};

/// Lifetime of one login session, matching the lifetime of an iLink QR code.
const SESSION_SECONDS: u64 = 480;

/// Type-erased login runtime moved into the Plugin's login task.
pub(crate) type LoginRuntime = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// Time limits applied by the login runtime.
#[derive(Clone, Copy)]
pub(crate) struct LoginTiming {
    /// Session deadline counted from the QR code being issued.
    session: Duration,
    /// Minimum spacing between status polls, in case iLink stops holding them.
    min_poll_interval: Duration,
}

impl LoginTiming {
    /// Limits used on the device.
    pub(crate) const DEVICE: Self = Self {
        session: Duration::from_secs(SESSION_SECONDS),
        min_poll_interval: Duration::from_secs(1),
    };
}

/// Last observed state of the login session.
enum SessionStatus {
    Idle,
    Wait,
    Scanned,
    Confirmed,
    Expired,
    Failed(String),
}

impl SessionStatus {
    const fn name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Wait => "wait",
            Self::Scanned => "scanned",
            Self::Confirmed => "confirmed",
            Self::Expired => "expired",
            Self::Failed(_) => "failed",
        }
    }

    fn message(&self) -> Option<&str> {
        match self {
            Self::Failed(message) => Some(message),
            _ => None,
        }
    }
}

/// Requests from the endpoint to the runtime. A newer command replaces an
/// unconsumed one, and both end the running session.
enum LoginCommand {
    Start,
    Cancel,
}

/// State shared by the endpoint and the runtime.
struct LoginShared {
    status: RefCell<SessionStatus>,
    commands: Signal<NoopRawMutex, LoginCommand>,
    /// Outcome of the QR request made for the latest `Start`.
    started: Signal<NoopRawMutex, Result<String, LoginError>>,
    /// Serializes start and cancel requests from concurrent connections.
    control: Mutex<NoopRawMutex, ()>,
    stopped: Cell<bool>,
}

impl LoginShared {
    fn set(&self, status: SessionStatus) {
        self.status.replace(status);
    }
}

/// Marks the runtime stopped and releases a waiting start request when the
/// runtime is dropped by Plugin cancellation.
struct StopOnDrop(Rc<LoginShared>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stopped.set(true);
        self.0.started.signal(Err(LoginError::Transport {
            message: "WeChat login service stopped".into(),
        }));
    }
}

/// Read-only view of the login session for the portal status.
pub(crate) struct LoginWatch(Rc<LoginShared>);

impl LoginWatch {
    /// A watch of a login service that never ran, for tests.
    #[cfg(test)]
    pub(crate) fn idle() -> Self {
        Self(Rc::new(LoginShared {
            status: RefCell::new(SessionStatus::Idle),
            commands: Signal::new(),
            started: Signal::new(),
            control: Mutex::new(()),
            stopped: Cell::new(false),
        }))
    }

    /// Returns whether a QR code is issued and not yet confirmed or ended.
    pub(crate) fn waiting_for_scan(&self) -> bool {
        matches!(
            *self.0.status.borrow(),
            SessionStatus::Wait | SessionStatus::Scanned
        )
    }
}

/// `POST`, `GET`, and `DELETE` on [`crate::LOGIN_API_PATH`].
pub(crate) struct LoginEndpoint<Storage, T: 'static, D: 'static, R: 'static> {
    shared: Rc<LoginShared>,
    configuration: Rc<ChannelConfiguration<Storage, T, D, R>>,
}

impl<Storage, T, D, R> LoginEndpoint<Storage, T, D, R>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
    R: TcpConnect + 'static,
{
    /// Creates the endpoint and the runtime future that must run as one task.
    ///
    /// `default_api_base` is used when no configuration is stored.
    pub(crate) fn new(
        configuration: Rc<ChannelConfiguration<Storage, T, D, R>>,
        default_api_base: String,
        timing: LoginTiming,
    ) -> (Self, LoginRuntime) {
        let shared = Rc::new(LoginShared {
            status: RefCell::new(SessionStatus::Idle),
            commands: Signal::new(),
            started: Signal::new(),
            control: Mutex::new(()),
            stopped: Cell::new(false),
        });
        let runner = LoginRunner {
            shared: Rc::clone(&shared),
            configuration: Rc::clone(&configuration),
            default_api_base,
            timing,
        };
        let endpoint = Self {
            shared,
            configuration,
        };
        (endpoint, Box::pin(runner.run()))
    }

    async fn start(&self, body: &[u8]) -> HttpResponse {
        if !body.trim_ascii().is_empty() && serde_json::from_slice::<StartBody>(body).is_err() {
            return json_response(400, br#"{"error":"invalid_request"}"#);
        }
        let _control = self.shared.control.lock().await;
        if self.shared.stopped.get() {
            return upstream_unavailable(Some("WeChat login service stopped"), None);
        }
        self.shared.started.reset();
        self.shared.commands.signal(LoginCommand::Start);
        match self.shared.started.wait().await {
            Ok(url) => to_json(
                200,
                &Started {
                    url: &url,
                    expires_in: SESSION_SECONDS,
                },
            ),
            Err(error) => upstream_unavailable(Some(error.message()), error.code()),
        }
    }

    /// Returns a handle reporting whether a session waits for its scan.
    pub(crate) fn watch(&self) -> LoginWatch {
        LoginWatch(Rc::clone(&self.shared))
    }

    async fn status(&self) -> HttpResponse {
        let configured = self.configuration.is_configured();
        let status = self.shared.status.borrow();
        to_json(
            200,
            &StatusBody {
                status: status.name(),
                configured,
                message: status.message(),
            },
        )
    }

    async fn cancel(&self) -> HttpResponse {
        let _control = self.shared.control.lock().await;
        self.shared.commands.signal(LoginCommand::Cancel);
        self.shared.set(SessionStatus::Idle);
        HttpResponse::new(204, JSON_CONTENT_TYPE, Vec::new())
    }
}

impl<Storage, T, D, R> HttpEndpoint for LoginEndpoint<Storage, T, D, R>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
    R: TcpConnect + 'static,
{
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Post => self.start(request.body()).await,
                HttpMethod::Get => self.status().await,
                HttpMethod::Delete => self.cancel().await,
                _ => json_response(405, br#"{"error":"method_not_allowed"}"#),
            }
        })
    }
}

/// The only accepted `POST` body besides an empty one: `{}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBody {}

#[derive(Serialize)]
struct Started<'a> {
    url: &'a str,
    expires_in: u64,
}

#[derive(Serialize)]
struct StatusBody<'a> {
    status: &'static str,
    configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
}

fn upstream_unavailable(message: Option<&str>, code: Option<&str>) -> HttpResponse {
    to_json(
        502,
        &ErrorBody {
            error: "upstream_unavailable",
            message,
            code,
        },
    )
}

fn to_json(status: u16, body: &impl Serialize) -> HttpResponse {
    let body = serde_json::to_vec(body).unwrap_or_else(|_| Vec::from(&b"{}"[..]));
    HttpResponse::new(status, JSON_CONTENT_TYPE, body)
}

/// Credentials of a confirmed login with the API base they belong to.
struct Confirmed {
    credentials: LoginCredentials,
    api_base: String,
}

struct LoginRunner<Storage, T: 'static, D: 'static, R: 'static> {
    shared: Rc<LoginShared>,
    configuration: Rc<ChannelConfiguration<Storage, T, D, R>>,
    default_api_base: String,
    timing: LoginTiming,
}

impl<Storage, T, D, R> LoginRunner<Storage, T, D, R>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
    R: TcpConnect + 'static,
{
    async fn run(self) {
        let _stopped = StopOnDrop(Rc::clone(&self.shared));
        let mut command = self.shared.commands.wait().await;
        loop {
            if let LoginCommand::Cancel = command {
                self.shared.set(SessionStatus::Idle);
                command = self.shared.commands.wait().await;
                continue;
            }
            // Boxed per session so the parked runtime does not reserve room
            // for an upstream request between sessions.
            let session = Box::pin(self.session());
            command = match select(self.shared.commands.wait(), session).await {
                Either::First(next) => next,
                Either::Second(confirmed) => {
                    if let Some(confirmed) = confirmed {
                        // Boxed for the same reason: storage futures are large.
                        Box::pin(self.confirm(confirmed)).await;
                    }
                    self.shared.commands.wait().await
                }
            };
        }
    }

    /// Issues a QR code, answers the waiting start request, and polls until
    /// the session ends. Returns credentials only for a confirmed login.
    async fn session(&self) -> Option<Confirmed> {
        self.shared.set(SessionStatus::Idle);
        let api_base = self
            .configuration
            .stored_api_base()
            .await
            .unwrap_or_else(|| self.default_api_base.clone());
        let login = WechatLogin::new(self.configuration.http_clients.clone(), api_base);
        let qrcode = match login.qrcode().await {
            Ok(qrcode) => qrcode,
            Err(error) => {
                log::warn!("failed to fetch a WeChat login QR code: {error}");
                self.shared.started.signal(Err(error));
                return None;
            }
        };
        self.shared.set(SessionStatus::Wait);
        self.shared.started.signal(Ok(qrcode.url));
        log::info!("started WeChat QR login session");
        if let Ok(confirmed) =
            with_timeout(self.timing.session, self.poll(&login, &qrcode.id)).await
        {
            confirmed
        } else {
            log::info!("WeChat QR login session timed out");
            self.shared.set(SessionStatus::Expired);
            None
        }
    }

    async fn poll(&self, login: &WechatLogin<'static, T, D>, qrcode: &str) -> Option<Confirmed> {
        loop {
            let polled = Instant::now();
            match login.status(qrcode).await {
                Ok(LoginStatus::Wait) => {}
                Ok(LoginStatus::Scanned) => self.shared.set(SessionStatus::Scanned),
                Ok(LoginStatus::Expired) => {
                    log::info!("WeChat login QR code expired");
                    self.shared.set(SessionStatus::Expired);
                    return None;
                }
                Ok(LoginStatus::Confirmed(credentials)) => {
                    return Some(Confirmed {
                        credentials,
                        api_base: login.api_base().into(),
                    });
                }
                Err(error) => {
                    log::warn!("WeChat login status poll failed: {error}");
                    self.shared.set(SessionStatus::Failed(error.to_string()));
                    return None;
                }
            }
            if let Some(rest) = self.timing.min_poll_interval.checked_sub(polled.elapsed()) {
                Timer::after(rest).await;
            }
        }
    }

    /// Stores and registers the confirmed bot through the configuration
    /// path, then restarts receiving with it.
    async fn confirm(&self, confirmed: Confirmed) {
        let Confirmed {
            credentials,
            api_base,
        } = confirmed;
        let api_base = credentials
            .base_url
            .filter(|base| base.starts_with("https://") || base.starts_with("http://"))
            .unwrap_or(api_base);
        let config = ConfigRequest::from_login(credentials.bot_token, api_base);
        let bot_id = credentials.bot_id.as_deref();
        let status = match self.configuration.link(config, bot_id).await {
            Ok(()) => {
                log::info!("configured WeChat channel from QR login");
                SessionStatus::Confirmed
            }
            Err(ConfigureError::Storage) => {
                SessionStatus::Failed("failed to store the WeChat configuration".into())
            }
            Err(ConfigureError::Registration) => {
                SessionStatus::Failed("the Gateway rejected the WeChat channel".into())
            }
        };
        self.shared.set(status);
    }
}

#[cfg(test)]
mod tests;

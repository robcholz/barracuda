//! QQ scan-to-bind: the HTTP endpoint and the Plugin-owned runtime.
//!
//! At most one bind session exists. The browser starts, observes, and cancels
//! it through [`LoginEndpoint`]; the runtime future, spawned once as the
//! Plugin's login task, makes every `q.qq.com` call. While no session runs,
//! the runtime is parked on its command signal and holds no connection.
//!
//! A completed binding hands over the bot's App ID and sealed App Secret. The
//! runtime opens the secret, verifies it like a typed one, stores it through
//! the configuration path, and makes the person who scanned an owner.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;

use barracuda_imessage_gateway_channel::ReceiveSlotSource;
use barracuda_plugin::api::Entropy;
use barracuda_plugin::manager::PluginStorage;
use barracuda_webserver_plugin::{HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse};
use embassy_futures::select::{select, Either};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use qq::bind::{connect_url, BindError, BindKey, BindStatus, BoundBot, QQBind};
use qq::QQ;
use serde::{Deserialize, Serialize};

use crate::channel::{ConfigureError, QQChannel};
use crate::{default_qq_api_base, default_qq_token_url, ConfigRequest, JSON_CONTENT_TYPE};

/// Longest a bind session waits for its binding; QQ may expire it sooner.
const SESSION_SECONDS: u64 = 600;

/// Type-erased login runtime moved into the Plugin's login task.
pub(crate) type LoginRuntime = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// Time limits applied by the login runtime.
#[derive(Clone, Copy)]
pub(crate) struct LoginTiming {
    /// Session deadline counted from the QR code being issued.
    pub(crate) session: Duration,
    /// Spacing between `poll_bind_result` calls.
    pub(crate) poll_interval: Duration,
}

impl LoginTiming {
    /// Limits used on the device: QQ's own clients poll every 2 s.
    pub(crate) const DEVICE: Self = Self {
        session: Duration::from_secs(SESSION_SECONDS),
        poll_interval: Duration::from_secs(2),
    };
}

/// Last observed state of the bind session.
enum SessionStatus {
    Idle,
    Wait,
    Confirmed,
    Expired,
    Failed(String),
}

impl SessionStatus {
    const fn name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Wait => "wait",
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

/// Why a session could not issue a QR code.
enum StartError {
    Entropy,
    Upstream(BindError),
}

/// State shared by the endpoint and the runtime.
struct LoginShared {
    status: RefCell<SessionStatus>,
    commands: Signal<NoopRawMutex, LoginCommand>,
    /// Outcome of the bind task created for the latest `Start`: its QR URL.
    started: Signal<NoopRawMutex, Result<String, StartError>>,
    /// Serializes start and cancel requests from concurrent connections.
    control: Mutex<NoopRawMutex, ()>,
    stopped: Cell<bool>,
}

impl LoginShared {
    fn new() -> Self {
        Self {
            status: RefCell::new(SessionStatus::Idle),
            commands: Signal::new(),
            started: Signal::new(),
            control: Mutex::new(()),
            stopped: Cell::new(false),
        }
    }

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
        self.0
            .started
            .signal(Err(StartError::Upstream(BindError::Transport {
                message: "QQ bind service stopped".into(),
            })));
    }
}

/// Read-only view of the bind session for the portal status.
pub(crate) struct LoginWatch(Rc<LoginShared>);

impl LoginWatch {
    /// Returns whether a QR code is issued and its binding not yet completed
    /// or ended.
    pub(crate) fn waiting_for_scan(&self) -> bool {
        matches!(*self.0.status.borrow(), SessionStatus::Wait)
    }
}

/// `POST`, `GET`, and `DELETE` on [`crate::LOGIN_API_PATH`].
pub(crate) struct LoginEndpoint<Storage, Slots: ReceiveSlotSource, T: 'static, D: 'static> {
    shared: Rc<LoginShared>,
    channel: Rc<QQChannel<Storage, Slots, T, D>>,
}

impl<Storage, Slots, T, D> LoginEndpoint<Storage, Slots, T, D>
where
    Storage: PluginStorage,
    Slots: ReceiveSlotSource,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    /// Creates the endpoint and the runtime future that must run as one task,
    /// calling the bind portal at `portal_base`.
    pub(crate) fn new(
        channel: Rc<QQChannel<Storage, Slots, T, D>>,
        portal_base: String,
        timing: LoginTiming,
    ) -> (Self, LoginRuntime) {
        let shared = Rc::new(LoginShared::new());
        let runner = LoginRunner {
            shared: Rc::clone(&shared),
            channel: Rc::clone(&channel),
            portal_base,
            timing,
        };
        (Self { shared, channel }, Box::pin(runner.run()))
    }

    /// Returns a handle reporting whether a session waits for its binding.
    pub(crate) fn watch(&self) -> LoginWatch {
        LoginWatch(Rc::clone(&self.shared))
    }

    async fn start(&self, body: &[u8]) -> HttpResponse {
        if !body.trim_ascii().is_empty() && serde_json::from_slice::<StartBody>(body).is_err() {
            return json_response(400, br#"{"error":"invalid_request"}"#);
        }
        let _control = self.shared.control.lock().await;
        if self.shared.stopped.get() {
            return upstream_unavailable(Some("QQ bind service stopped"), None);
        }
        self.shared.started.reset();
        self.shared.commands.signal(LoginCommand::Start);
        match self.shared.started.wait().await {
            Ok(url) => to_json(200, &Started { url: &url }),
            Err(StartError::Entropy) => json_response(500, br#"{"error":"entropy_unavailable"}"#),
            Err(StartError::Upstream(error)) => {
                upstream_unavailable(Some(error.message()), error.code())
            }
        }
    }

    fn status(&self) -> HttpResponse {
        let status = self.shared.status.borrow();
        let settings = self.channel.settings();
        to_json(
            200,
            &StatusBody {
                status: status.name(),
                configured: settings.is_some(),
                app_id: settings
                    .as_ref()
                    .map(|settings| settings.config.app_id.as_str()),
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

impl<Storage, Slots, T, D> HttpEndpoint for LoginEndpoint<Storage, Slots, T, D>
where
    Storage: PluginStorage,
    Slots: ReceiveSlotSource,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Post => self.start(request.body()).await,
                HttpMethod::Get => self.status(),
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
}

#[derive(Serialize)]
struct StatusBody<'a> {
    status: &'static str,
    configured: bool,
    /// The configured bot, for the page's 「QQ 已绑定」 card; never the secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    app_id: Option<&'a str>,
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

fn json_response(status: u16, body: &'static [u8]) -> HttpResponse {
    HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
}

fn to_json(status: u16, body: &impl Serialize) -> HttpResponse {
    let body = serde_json::to_vec(body).unwrap_or_else(|_| Vec::from(&b"{}"[..]));
    HttpResponse::new(status, JSON_CONTENT_TYPE, body)
}

struct LoginRunner<Storage, Slots: ReceiveSlotSource, T: 'static, D: 'static> {
    shared: Rc<LoginShared>,
    channel: Rc<QQChannel<Storage, Slots, T, D>>,
    portal_base: String,
    timing: LoginTiming,
}

impl<Storage, Slots, T, D> LoginRunner<Storage, Slots, T, D>
where
    Storage: PluginStorage,
    Slots: ReceiveSlotSource,
    T: TcpConnect + 'static,
    D: Dns + 'static,
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
                Either::Second(bound) => {
                    if let Some(bound) = bound {
                        // Boxed for the same reason: verification and storage
                        // futures are large.
                        Box::pin(self.confirm(bound)).await;
                    }
                    self.shared.commands.wait().await
                }
            };
        }
    }

    /// Creates a bind task, answers the waiting start request with its QR
    /// URL, and polls until the session ends. Returns the bot only for a
    /// completed binding.
    async fn session(&self) -> Option<BoundBot> {
        self.shared.set(SessionStatus::Idle);
        let mut key = [0_u8; 32];
        if self.channel.entropy.fill(&mut key).is_err() {
            log::warn!("no entropy for a QQ bind key");
            self.shared.started.signal(Err(StartError::Entropy));
            return None;
        }
        let key = BindKey::new(key);
        let bind = QQBind::new(self.channel.http_clients.clone(), self.portal_base.as_str());
        let task = match bind.create(&key).await {
            Ok(task) => task,
            Err(error) => {
                log::warn!("failed to create a QQ bind task: {error}");
                self.shared.started.signal(Err(StartError::Upstream(error)));
                return None;
            }
        };
        self.shared.set(SessionStatus::Wait);
        self.shared.started.signal(Ok(connect_url(&task)));
        log::info!("started QQ bind session");
        if let Ok(bound) = with_timeout(self.timing.session, self.poll(&bind, &task, &key)).await {
            bound
        } else {
            log::info!("QQ bind session timed out");
            self.shared.set(SessionStatus::Expired);
            None
        }
    }

    async fn poll(
        &self,
        bind: &QQBind<'static, T, D>,
        task: &str,
        key: &BindKey,
    ) -> Option<BoundBot> {
        loop {
            let polled = Instant::now();
            match bind.poll(task, key).await {
                Ok(BindStatus::Waiting) => {}
                Ok(BindStatus::Expired) => {
                    log::info!("QQ bind task expired");
                    self.shared.set(SessionStatus::Expired);
                    return None;
                }
                Ok(BindStatus::Completed(bot)) => return Some(bot),
                // A dropped connection or a garbled reply says nothing about
                // the task: ask again.
                Err(BindError::Transport { message }) => {
                    log::warn!("QQ bind poll failed: {message}");
                }
                Err(error) => {
                    log::warn!("QQ bind poll failed: {error}");
                    self.shared.set(SessionStatus::Failed(error.to_string()));
                    return None;
                }
            }
            if let Some(rest) = self.timing.poll_interval.checked_sub(polled.elapsed()) {
                Timer::after(rest).await;
            }
        }
    }

    /// Verifies the bound bot's credentials, stores them through the
    /// configuration path, and makes the person who bound it an owner.
    async fn confirm(&self, bot: BoundBot) {
        let BoundBot {
            app_id,
            app_secret,
            user_openid,
        } = bot;
        let (api_base, token_url) = self.channel.settings().map_or_else(
            || (default_qq_api_base(), default_qq_token_url()),
            |settings| {
                (
                    settings.config.api_base.clone(),
                    settings.config.token_url.clone(),
                )
            },
        );
        let config = ConfigRequest {
            app_id,
            app_secret,
            api_base,
            token_url,
        };
        let sender = Rc::new(QQ::new(
            self.channel.http_clients.clone(),
            config.clone().into(),
        ));
        if let Err(error) = sender.authenticate().await {
            log::warn!("QQ refused the bound bot's credentials: {error}");
            self.shared.set(SessionStatus::Failed(error.to_string()));
            return;
        }
        let status = match self.channel.configure(config, sender).await {
            Ok(()) => {
                log::info!("configured QQ channel from a QR binding");
                if let Some(openid) = user_openid {
                    match self.channel.add_owner(&openid).await {
                        Ok(true) => log::info!("made the QQ binding's scanner an owner"),
                        Ok(false) => {}
                        Err(error) => {
                            log::warn!("failed to store the QQ scanner as an owner: {error}")
                        }
                    }
                }
                SessionStatus::Confirmed
            }
            Err(ConfigureError::Storage) => {
                SessionStatus::Failed("failed to store the QQ configuration".into())
            }
            Err(ConfigureError::Registration) => {
                SessionStatus::Failed("the Gateway rejected the QQ channel".into())
            }
        };
        self.shared.set(status);
    }
}

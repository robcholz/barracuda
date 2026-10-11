//! IMessage Inkbox provider Plugin.

#![no_std]
#![recursion_limit = "256"]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod receive;
#[cfg(test)]
mod tests;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use barracuda_captive_portal_plugin::{
    CaptivePortal, EntryState, EntryStatus, ResourceFiles, WebEntry, WebGroup, WebText,
};
use barracuda_imessage_gateway_plugin::{
    ChannelControl, ChannelLevel, ChannelMode, ChannelStatus, GatewayError, IMessageGateway,
    MessageChannel, MessageChannelRegistration, ModeError, ModeFuture, OnDemand, Owners,
    OwnersError, ReceiveControl, ReceiveRuntime, ReceiveSlots, ReceiveTiming,
};
use barracuda_plugin::api::PluginContext;
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
use embassy_time::Duration;
use inkbox::{Inkbox, InkboxConfig, InkboxSignup, SignupAccount, SignupError, SignupRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::receive::{Cursor, CURSOR_STORAGE_KEY, POLL_INTERVAL};

/// HTTP path accepting Inkbox configuration; the Gateway serves the
/// channel's `/status`, `/mode`, and `/owners` below it.
pub const CONFIG_API_PATH: &str = "/api/gateway/inkbox";
/// HTTP path starting Inkbox agent self-signup for an email address.
pub const SIGNUP_API_PATH: &str = "/api/gateway/inkbox/signup";
/// HTTP path submitting the emailed Inkbox verification code.
pub const VERIFY_API_PATH: &str = "/api/gateway/inkbox/verify";
/// HTTP path asking Inkbox to resend the verification email.
pub const RESEND_API_PATH: &str = "/api/gateway/inkbox/resend";

const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";
const SIGNUP_NOTE_TO_HUMAN: &str = "Barracuda, your device, set up an Inkbox mailbox to send and receive mail for you. Enter the code from this email on the device portal.";
const SIGNUP_DISPLAY_NAME: &str = "Barracuda";
const SIGNUP_HARNESS: &str = "barracuda";
const VERIFICATION_CODE_DIGITS: usize = 6;

/// Plugin that exposes Inkbox configuration, registers the resulting
/// channel, and polls it for inbound iMessages.
#[barracuda_plugin::macros::plugin]
pub struct IMessageInkboxPlugin {
    runtime: Option<ReceiveRuntime>,
}

impl IMessageInkboxPlugin {
    /// Creates the unregistered Inkbox channel Plugin.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { runtime: None }
    }
}

impl Plugin for IMessageInkboxPlugin {
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
        let (state, runtime) = build_state(
            context.storage().clone(),
            gateway,
            inkbox::DEFAULT_API_BASE.into(),
            Timing::DEVICE,
        )?;
        let status = Rc::clone(&state);
        context.retain(
            portal
                .register_with_status(
                    WebEntry {
                        id: "imessage-inkbox",
                        group: WebGroup::Channel,
                        order: 60,
                        title: WebText {
                            zh: "Inkbox",
                            en: "Inkbox",
                        },
                        summary: WebText {
                            zh: "Inkbox 身份与邮件服务",
                            en: "Inkbox identity and mail",
                        },
                        icon: Some("icon.png"),
                        figure: None,
                        module: "entry.js",
                    },
                    ResourceFiles::from(context.filesystem()?.clone()),
                    move || entry_status(status.gateway.channel_status(&*status)),
                )
                .map_err(PluginError::registration)?,
        );
        context.retain(serve_routes(&state, &webserver).map_err(PluginError::registration)?);
        self.runtime = Some(runtime);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(ReceiveRuntimeUnavailable))?;
        let task = inkbox_receive_task(runtime, context.task_token())
            .map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

/// Owns the receive loop; parked without a slot unless the channel is
/// configured and in `send_receive`.
#[embassy_executor::task]
async fn inkbox_receive_task(runtime: ReceiveRuntime, cancellation: PluginTaskToken) {
    let _completed = select(cancellation.cancelled(), runtime).await;
    log::info!("stopped Inkbox receive task");
}

/// Serves the channel's routes: the Gateway serves the configuration path
/// with its `/status`, `/mode`, and `/owners`; the exact sign-up routes below
/// it take their own requests.
fn serve_routes<Storage: PluginStorage>(
    state: &Rc<InkboxState<Storage>>,
    webserver: &WebServer,
) -> Result<Vec<WebRouteRegistration>, WebServerError> {
    let endpoint = |route| InkboxEndpoint {
        state: Rc::clone(state),
        route,
    };
    let mut registrations = Vec::with_capacity(4);
    registrations.push(state.gateway.serve_channel(
        CONFIG_API_PATH,
        Rc::clone(state),
        endpoint(Route::Config),
    )?);
    for (path, route) in [
        (SIGNUP_API_PATH, Route::Signup),
        (VERIFY_API_PATH, Route::Verify),
        (RESEND_API_PATH, Route::Resend),
    ] {
        registrations.push(webserver.serve_http(path, endpoint(route))?);
    }
    Ok(registrations)
}

/// The channel's portal card status.
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

#[derive(Debug)]
struct ReceiveRuntimeUnavailable;

impl core::fmt::Display for ReceiveRuntimeUnavailable {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Inkbox receive runtime was not prepared during Plugin registration")
    }
}

impl core::error::Error for ReceiveRuntimeUnavailable {}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigRequest {
    api_key: String,
    identity_id: String,
    #[serde(default = "default_inkbox_api_base")]
    api_base: String,
}

/// Stored channel configuration: the accepted settings and, when they came
/// from signup, the account they belong to. Older records have no `signup`.
#[derive(Clone, Deserialize, Serialize)]
struct StoredConfiguration {
    api_key: String,
    identity_id: String,
    api_base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signup: Option<SignupRecord>,
}

/// Account details of a signup, reported by `GET /status` without the key.
#[derive(Clone, Deserialize, Serialize)]
struct SignupRecord {
    /// Address the person entered, which Inkbox sent the code to.
    human_email: String,
    email_address: String,
    /// Last claim status Inkbox reported, from signup or a later verify.
    claim_status: String,
}

impl From<ConfigRequest> for StoredConfiguration {
    fn from(value: ConfigRequest) -> Self {
        Self {
            api_key: value.api_key,
            identity_id: value.identity_id,
            api_base: value.api_base,
            signup: None,
        }
    }
}

impl StoredConfiguration {
    /// Returns the channel settings, leaving out the signup record.
    fn inkbox_config(&self) -> InkboxConfig {
        InkboxConfig {
            api_key: self.api_key.clone(),
            identity_id: self.identity_id.clone(),
            api_base: self.api_base.clone(),
        }
    }

    /// Whether `other` reads the same Inkbox identity's messages.
    fn same_inbox(&self, other: &Self) -> bool {
        self.api_key == other.api_key
            && self.identity_id == other.identity_id
            && self.api_base == other.api_base
    }
}

/// A signup whose one-time API key is not stored yet. It is kept only in RAM,
/// so a retry for the same email resumes without a second signup email.
struct PendingSignup {
    human_email: String,
    account: SignupAccount,
    identity_id: Option<String>,
}

fn default_inkbox_api_base() -> String {
    inkbox::DEFAULT_API_BASE.into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignupBody {
    email: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyBody {
    code: String,
}

/// A stored configuration and the provider built from it.
pub(crate) struct Settings {
    pub(crate) config: StoredConfiguration,
    /// Sends through the shared request pool, never a receive slot.
    pub(crate) sender: Rc<dyn MessageChannel>,
}

/// Builds the sending provider for a configuration.
type Connect = Box<dyn Fn(InkboxConfig) -> Rc<dyn MessageChannel>>;

/// Channel state shared by every Inkbox HTTP route, the portal status, and
/// the receive loop.
pub(crate) struct InkboxState<Storage> {
    pub(crate) gateway: Rc<IMessageGateway>,
    connect: Connect,
    settings: RefCell<Option<Rc<Settings>>>,
    mode: Cell<ChannelMode>,
    /// Present exactly while configured and the mode registers.
    registration: RefCell<Option<MessageChannelRegistration>>,
    receive: Rc<ReceiveControl<ReceiveSlots>>,
    /// Loaded once the channel is configured.
    owners: OnDemand<Owners<Storage>>,
    /// Loaded by the first receive session.
    pub(crate) cursor: OnDemand<Cursor>,
    /// Time between two polls of a healthy connection.
    pub(crate) poll_interval: Duration,
    /// Serializes configuration, mode, and claim-status changes.
    changes: Mutex<NoopRawMutex, ()>,
    /// Admits one signup, verify, or resend flow at a time, so the Plugin
    /// opens at most one upstream connection for them.
    upstream: Mutex<NoopRawMutex, ()>,
    /// Signup result that failed to be stored or registered, until it is.
    pending_signup: RefCell<Option<PendingSignup>>,
    pub(crate) storage: Storage,
    /// Inkbox service origin used for signup and stored with its result.
    signup_api_base: String,
}

/// Time limits of the receive loop.
#[derive(Clone, Copy)]
pub(crate) struct Timing {
    /// Reconnect backoff and slot retry.
    pub(crate) receive: ReceiveTiming,
    /// Time between two polls of a healthy connection.
    pub(crate) poll: Duration,
}

impl Timing {
    /// Limits used on the device.
    pub(crate) const DEVICE: Self = Self {
        receive: ReceiveTiming::DEVICE,
        poll: POLL_INTERVAL,
    };
}

/// The state and the receive runtime its task owns.
pub(crate) type Built<Storage> = (Rc<InkboxState<Storage>>, ReceiveRuntime);

/// Loads the channel from storage, registers it with the Gateway when its
/// mode asks for it, and builds the receive runtime the Plugin's task owns.
pub(crate) fn build_state<Storage: PluginStorage>(
    storage: Storage,
    gateway: Rc<IMessageGateway>,
    signup_api_base: String,
    timing: Timing,
) -> PluginResult<Built<Storage>> {
    let config = embassy_futures::block_on(load_configuration(&storage))?;
    let mode = embassy_futures::block_on(gateway.load_mode(
        &storage,
        config.is_some(),
        ChannelMode::legacy(),
    ))?;
    let http = gateway.http();
    let connect: Connect = Box::new(move |config| {
        let sender: Rc<dyn MessageChannel> = Rc::new(Inkbox::new(http.clone(), config));
        sender
    });
    let settings = config.map(|config| {
        Rc::new(Settings {
            sender: connect(config.inkbox_config()),
            config,
        })
    });
    let receive = Rc::new(gateway.receive_control());
    let state = Rc::new(InkboxState {
        gateway,
        connect,
        settings: RefCell::new(settings),
        mode: Cell::new(mode),
        registration: RefCell::new(None),
        receive: Rc::clone(&receive),
        owners: OnDemand::new(),
        cursor: OnDemand::new(),
        poll_interval: timing.poll,
        changes: Mutex::new(()),
        upstream: Mutex::new(()),
        pending_signup: RefCell::new(None),
        storage,
        signup_api_base,
    });
    if state.configured() {
        embassy_futures::block_on(state.load_owners()).map_err(PluginError::registration)?;
    }
    state
        .register_for(mode)
        .map_err(PluginError::registration)?;
    let runtime = state
        .gateway
        .receive_runtime(receive, Rc::clone(&state), timing.receive);
    if state.gateway.sync_receive(&*state).is_err() {
        log::warn!("Inkbox waits for a free receive slot");
    }
    Ok((state, runtime))
}

enum ApplyError {
    Storage,
    Rejected,
}

enum StoredSignupError {
    Missing,
    Storage,
}

impl StoredSignupError {
    fn response(self) -> HttpResponse {
        match self {
            Self::Missing => error_response(
                409,
                "conflict",
                None,
                Some("no Inkbox signup is stored on this device".into()),
            ),
            Self::Storage => static_response(500, br#"{"error":"storage"}"#),
        }
    }
}

impl<Storage: PluginStorage> InkboxState<Storage> {
    /// The current configuration and its provider.
    pub(crate) fn settings(&self) -> Option<Rc<Settings>> {
        self.settings.borrow().clone()
    }

    /// Registers or unregisters with the Gateway so that a registration
    /// exists exactly while configured and `mode` registers.
    fn register_for(&self, mode: ChannelMode) -> Result<(), GatewayError> {
        let wanted = self.settings().filter(|_settings| mode.registers());
        let mut registration = self.registration.borrow_mut();
        match (wanted, registration.is_some()) {
            (Some(settings), false) => {
                *registration = Some(self.gateway.register(Rc::clone(&settings.sender))?);
            }
            (None, true) => {
                registration.take();
            }
            _ => {}
        }
        Ok(())
    }

    async fn configure(&self, body: &[u8]) -> HttpResponse {
        let Ok(config) = serde_json::from_slice::<ConfigRequest>(body) else {
            log::warn!("rejected invalid Inkbox gateway configuration");
            return static_response(400, br#"{"error":"invalid_request"}"#);
        };
        match self.apply(config.into()).await {
            Ok(()) => static_response(204, b""),
            Err(ApplyError::Storage) => static_response(500, br#"{"error":"storage"}"#),
            Err(ApplyError::Rejected) => {
                static_response(422, br#"{"error":"registration_failed"}"#)
            }
        }
    }

    async fn sign_up(&self, body: &[u8]) -> HttpResponse {
        let Ok(request) = serde_json::from_slice::<SignupBody>(body) else {
            log::warn!("rejected invalid Inkbox signup request");
            return static_response(400, br#"{"error":"invalid_request"}"#);
        };
        let email = request.email.trim();
        if email.is_empty() {
            return error_response(
                400,
                "invalid_request",
                None,
                Some("email is required".into()),
            );
        }
        let Ok(_flow) = self.upstream.try_lock() else {
            return busy_response();
        };
        let client = InkboxSignup::new(self.gateway.http(), self.signup_api_base.as_str());
        // A kept result for another email is dropped: that person asked to start over.
        let pending = self
            .pending_signup
            .borrow_mut()
            .take()
            .filter(|pending| pending.human_email == email);
        let mut pending = if let Some(pending) = pending {
            log::info!("resuming the Inkbox signup kept from a failed attempt");
            pending
        } else {
            match client
                .sign_up(&SignupRequest {
                    human_email: email,
                    note_to_human: SIGNUP_NOTE_TO_HUMAN,
                    display_name: Some(SIGNUP_DISPLAY_NAME),
                    harness: Some(SIGNUP_HARNESS),
                })
                .await
            {
                Ok(account) => PendingSignup {
                    human_email: email.into(),
                    account,
                    identity_id: None,
                },
                Err(error) => {
                    log::warn!("Inkbox signup failed: {error}");
                    return upstream_failure(error, 400, "invalid_request", false);
                }
            }
        };
        let identity_id = if let Some(identity_id) = &pending.identity_id {
            identity_id.clone()
        } else {
            match client
                .resolve_identity(&pending.account.api_key, &pending.account.agent_handle)
                .await
            {
                Ok(identity_id) => {
                    pending.identity_id = Some(identity_id.clone());
                    identity_id
                }
                Err(error) => {
                    log::error!("failed to resolve the Inkbox signup identity: {error}");
                    self.pending_signup.replace(Some(pending));
                    return upstream_failure(error, 502, "upstream_unavailable", true);
                }
            }
        };
        let signup = SignupRecord {
            human_email: pending.human_email.clone(),
            email_address: pending.account.email_address.clone(),
            claim_status: pending.account.claim_status.clone(),
        };
        let config = StoredConfiguration {
            api_key: pending.account.api_key.clone(),
            identity_id,
            api_base: self.signup_api_base.clone(),
            signup: Some(signup.clone()),
        };
        let failure = match self.apply_signup(config).await {
            Ok(()) => {
                return json_response(
                    200,
                    &json!({
                        "email_address": signup.email_address,
                        "claim_status": signup.claim_status,
                    }),
                );
            }
            Err(ApplyError::Storage) => error_reply(500, "storage", None, None, true),
            Err(ApplyError::Rejected) => error_reply(422, "registration_failed", None, None, true),
        };
        self.pending_signup.replace(Some(pending));
        failure
    }

    async fn verify(&self, body: &[u8]) -> HttpResponse {
        let Ok(request) = serde_json::from_slice::<VerifyBody>(body) else {
            log::warn!("rejected invalid Inkbox verification request");
            return static_response(400, br#"{"error":"invalid_request"}"#);
        };
        let code = request.code.trim();
        if code.len() != VERIFICATION_CODE_DIGITS || !code.bytes().all(|byte| byte.is_ascii_digit())
        {
            return error_response(
                400,
                "invalid_request",
                None,
                Some("code must be 6 digits".into()),
            );
        }
        let config = match self.stored_configuration().await {
            Ok(config) => config,
            Err(error) => return error.response(),
        };
        let Ok(_flow) = self.upstream.try_lock() else {
            return busy_response();
        };
        match InkboxSignup::new(self.gateway.http(), config.api_base.as_str())
            .verify(&config.api_key, code)
            .await
        {
            Ok(claim_status) => {
                log::info!("verified Inkbox signup");
                self.record_claim_status(&config.api_key, &claim_status)
                    .await;
                json_response(200, &json!({ "claim_status": claim_status }))
            }
            Err(error) => {
                log::warn!("Inkbox signup verification failed: {error}");
                upstream_failure(error, 422, "verification_failed", false)
            }
        }
    }

    async fn resend(&self) -> HttpResponse {
        let config = match self.stored_configuration().await {
            Ok(config) => config,
            Err(error) => return error.response(),
        };
        let Ok(_flow) = self.upstream.try_lock() else {
            return busy_response();
        };
        match InkboxSignup::new(self.gateway.http(), config.api_base)
            .resend_verification(&config.api_key)
            .await
        {
            Ok(()) => static_response(204, b""),
            Err(error) => {
                log::warn!("Inkbox verification resend failed: {error}");
                upstream_failure(error, 422, "verification_failed", false)
            }
        }
    }

    /// Stores the claim status a verify returned, if the stored configuration
    /// is still the signup that `api_key` belongs to.
    async fn record_claim_status(&self, api_key: &str, claim_status: &str) {
        let _change = self.changes.lock().await;
        let Ok(mut config) = self.stored_configuration().await else {
            return;
        };
        let Some(signup) = config.signup.as_mut().filter(|_| config.api_key == api_key) else {
            return;
        };
        signup.claim_status = claim_status.into();
        let Ok(bytes) = encode_configuration(&config) else {
            log::error!("failed to encode the Inkbox claim status");
            return;
        };
        if let Err(error) = self
            .storage
            .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
            .await
        {
            log::error!("failed to store the Inkbox claim status: {error}");
            return;
        }
        let current = self.settings();
        if let Some(current) = current.filter(|current| current.config.same_inbox(&config)) {
            self.settings.replace(Some(Rc::new(Settings {
                sender: Rc::clone(&current.sender),
                config,
            })));
        }
    }

    /// Reads the stored configuration whose API key verify and resend use.
    async fn stored_configuration(&self) -> Result<StoredConfiguration, StoredSignupError> {
        let bytes = match self.storage.get_bytes(CONFIGURATION_STORAGE_KEY).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Err(StoredSignupError::Missing),
            Err(error) => {
                log::error!("failed to read the Inkbox gateway configuration: {error}");
                return Err(StoredSignupError::Storage);
            }
        };
        decode_configuration(&bytes).map_err(|_| {
            log::error!("stored Inkbox gateway configuration is malformed");
            StoredSignupError::Storage
        })
    }

    /// Loads the owner book, once; a configured channel needs it.
    async fn load_owners(&self) -> Result<&Owners<Storage>, OwnersError> {
        self.owners
            .get_or_load(|| self.gateway.load_owners(self.storage.clone()))
            .await
    }

    /// Applies a configuration from signup. A signup on a device without an
    /// Inkbox channel starts it sending and receiving.
    async fn apply_signup(&self, config: StoredConfiguration) -> Result<(), ApplyError> {
        if !self.configured() && self.mode.get() != ChannelMode::SendReceive {
            let _change = self.changes.lock().await;
            self.gateway
                .store_mode(&self.storage, ChannelMode::SendReceive)
                .await
                .map_err(|error| {
                    log::error!("failed to store the Inkbox mode: {error}");
                    ApplyError::Storage
                })?;
            self.mode.set(ChannelMode::SendReceive);
        }
        self.apply(config).await
    }

    /// Persists `config` and replaces the registered channel with it, restoring
    /// the previous stored configuration when the Gateway rejects the channel.
    async fn apply(&self, config: StoredConfiguration) -> Result<(), ApplyError> {
        let _change = self.changes.lock().await;
        if let Err(error) = self.load_owners().await {
            log::error!("failed to read the Inkbox owners: {error}");
            return Err(ApplyError::Storage);
        }
        let previous_configuration = match self.storage.get_bytes(CONFIGURATION_STORAGE_KEY).await {
            Ok(configuration) => configuration,
            Err(error) => {
                log::error!("failed to read the previous Inkbox gateway configuration: {error}");
                return Err(ApplyError::Storage);
            }
        };
        let Ok(bytes) = encode_configuration(&config) else {
            log::error!("failed to encode Inkbox gateway configuration");
            return Err(ApplyError::Storage);
        };
        if let Err(error) = self
            .storage
            .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
            .await
        {
            log::error!("failed to persist Inkbox gateway configuration: {error}");
            return Err(ApplyError::Storage);
        }
        let same_inbox = self
            .settings()
            .is_some_and(|settings| settings.config.same_inbox(&config));
        let settings = Rc::new(Settings {
            sender: (self.connect)(config.inkbox_config()),
            config,
        });
        self.registration.take();
        self.settings.replace(Some(settings));
        if let Err(error) = self.register_for(self.mode.get()) {
            self.settings.replace(None);
            let _no_slot = self.gateway.sync_receive(self);
            let restored = if let Some(previous) = previous_configuration.as_deref() {
                self.storage.put(CONFIGURATION_STORAGE_KEY, previous).await
            } else {
                self.storage.delete(CONFIGURATION_STORAGE_KEY).await
            };
            if let Err(storage_error) = restored {
                log::error!(
                    "failed to roll back rejected Inkbox gateway configuration: {storage_error}"
                );
                return Err(ApplyError::Storage);
            }
            log::warn!("rejected Inkbox gateway configuration: {error}");
            return Err(ApplyError::Rejected);
        }
        if !same_inbox {
            // Another identity's messages: start from its newest one.
            let forgotten = match self.cursor.get() {
                Some(cursor) => cursor.forget(&self.storage).await,
                None => self.storage.delete(CURSOR_STORAGE_KEY).await,
            };
            if let Err(error) = forgotten {
                log::warn!("failed to clear the Inkbox receive cursor: {error}");
            }
        }
        if self.gateway.sync_receive(self).is_err() {
            log::warn!("Inkbox waits for a free receive slot");
        }
        self.receive.restart();
        log::info!("configured Inkbox gateway provider");
        Ok(())
    }
}

impl<Storage: PluginStorage> ChannelControl for InkboxState<Storage> {
    type Storage = Storage;
    type Slots = ReceiveSlots;

    fn configured(&self) -> bool {
        self.settings.borrow().is_some()
    }

    fn mode(&self) -> ChannelMode {
        self.mode.get()
    }

    fn apply_mode(&self, mode: ChannelMode) -> ModeFuture<'_> {
        Box::pin(async move {
            let _change = self.changes.lock().await;
            let previous = self.mode.get();
            self.gateway
                .store_mode(&self.storage, mode)
                .await
                .map_err(|error| {
                    log::error!("failed to store the Inkbox mode: {error}");
                    ModeError::Storage
                })?;
            if let Err(error) = self.register_for(mode) {
                log::warn!("the Gateway rejected the Inkbox channel: {error}");
                if let Err(error) = self.gateway.store_mode(&self.storage, previous).await {
                    log::error!("failed to restore the Inkbox mode: {error}");
                }
                return Err(ModeError::Registration);
            }
            self.mode.set(mode);
            Ok(())
        })
    }

    fn receive(&self) -> &ReceiveControl<ReceiveSlots> {
        &self.receive
    }

    fn owners(&self) -> Option<&Owners<Storage>> {
        self.owners.get()
    }

    /// `signup`: the account of a configuration that came from signup; the
    /// API key is never reported.
    fn status_details(&self) -> Map<String, Value> {
        let mut details = Map::new();
        let signup = self
            .settings()
            .and_then(|settings| settings.config.signup.clone())
            .and_then(|signup| serde_json::to_value(signup).ok());
        if let Some(signup) = signup {
            details.insert("signup".into(), signup);
        }
        details
    }
}

#[derive(Clone, Copy)]
enum Route {
    Config,
    Signup,
    Verify,
    Resend,
}

/// One registered Inkbox HTTP route.
struct InkboxEndpoint<Storage> {
    state: Rc<InkboxState<Storage>>,
    route: Route,
}

impl<Storage: PluginStorage> HttpEndpoint for InkboxEndpoint<Storage> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match (request.method(), self.route) {
                (HttpMethod::Post, _) => {}
                _ => return static_response(405, br#"{"error":"method_not_allowed"}"#),
            }
            match self.route {
                Route::Config => self.state.configure(request.body()).await,
                Route::Signup => self.state.sign_up(request.body()).await,
                Route::Verify => self.state.verify(request.body()).await,
                Route::Resend => self.state.resend().await,
            }
        })
    }
}

fn static_response(status: u16, body: &'static [u8]) -> HttpResponse {
    HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
}

fn json_response(status: u16, body: &Value) -> HttpResponse {
    HttpResponse::new(
        status,
        JSON_CONTENT_TYPE,
        serde_json::to_vec(body).unwrap_or_default(),
    )
}

fn error_response(
    status: u16,
    kind: &str,
    code: Option<String>,
    message: Option<String>,
) -> HttpResponse {
    error_reply(status, kind, code, message, false)
}

/// Error body `{"error", "message"?, "code"?, "retry"?}`. `retry` is `true` when
/// the device kept a signup result that the same request can resume.
fn error_reply(
    status: u16,
    kind: &str,
    code: Option<String>,
    message: Option<String>,
    retry: bool,
) -> HttpResponse {
    let mut body = Map::new();
    body.insert("error".into(), kind.into());
    if let Some(message) = message {
        body.insert("message".into(), message.into());
    }
    if let Some(code) = code {
        body.insert("code".into(), code.into());
    }
    if retry {
        body.insert("retry".into(), true.into());
    }
    json_response(status, &Value::Object(body))
}

fn busy_response() -> HttpResponse {
    error_response(
        409,
        "conflict",
        None,
        Some("another Inkbox request is in progress".into()),
    )
}

/// Maps an Inkbox 4xx to `rejected_status`/`rejected_kind` and every other
/// failure to `502 upstream_unavailable`, passing Inkbox's code and message through.
fn upstream_failure(
    error: SignupError,
    rejected_status: u16,
    rejected_kind: &str,
    retry: bool,
) -> HttpResponse {
    match error {
        SignupError::Rejected { code, message, .. } => {
            error_reply(rejected_status, rejected_kind, code, message, retry)
        }
        SignupError::Unavailable { code, message } => {
            error_reply(502, "upstream_unavailable", code, message, retry)
        }
    }
}

fn encode_configuration(config: &StoredConfiguration) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(config)
}

fn decode_configuration(bytes: &[u8]) -> Result<StoredConfiguration, serde_json::Error> {
    serde_json::from_slice(bytes)
}

async fn load_configuration<Storage: PluginStorage>(
    storage: &Storage,
) -> PluginResult<Option<StoredConfiguration>> {
    storage
        .get_bytes(CONFIGURATION_STORAGE_KEY)
        .await?
        .map(|bytes| decode_configuration(&bytes))
        .transpose()
        .map_err(PluginError::registration)
}

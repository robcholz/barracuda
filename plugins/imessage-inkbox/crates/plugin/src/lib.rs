//! IMessage Inkbox provider Plugin.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

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
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use http_client::ClientFactory;
use inkbox::{Inkbox, InkboxConfig, InkboxSignup, SignupAccount, SignupError, SignupRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// HTTP path accepting Inkbox configuration.
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

/// Plugin that exposes Inkbox configuration and registers the resulting channel.
#[barracuda_plugin::macros::plugin]
pub struct IMessageInkboxPlugin {
    http_clients: ClientFactory<'static>,
}

impl IMessageInkboxPlugin {
    /// Creates an unconfigured provider using Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
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
        let gateway = context.require::<IMessageGateway>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let webserver = context.require::<WebServer>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[1],
        )?;
        let channel_registration =
            embassy_futures::block_on(load_configuration(context.storage()))?
                .map(|config| {
                    let channel: Rc<dyn MessageChannel> = Rc::new(Inkbox::new(
                        self.http_clients.clone(),
                        config.into_inkbox_config(),
                    ));
                    gateway.register(channel).map_err(PluginError::registration)
                })
                .transpose()?;
        let state = Rc::new(InkboxState {
            gateway,
            http_clients: self.http_clients.clone(),
            configured: Cell::new(channel_registration.is_some()),
            channel_registration: Mutex::new(channel_registration),
            upstream: Mutex::new(()),
            pending_signup: RefCell::new(None),
            storage: context.storage().clone(),
            signup_api_base: inkbox::DEFAULT_API_BASE.into(),
        });
        let status = Rc::clone(&state);
        let portal = context.require::<CaptivePortal>("captive-portal")?;
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
                    move || EntryStatus::configured(status.configured.get()),
                )
                .map_err(PluginError::registration)?,
        );
        for (path, route) in [
            (CONFIG_API_PATH, Route::Config),
            (SIGNUP_API_PATH, Route::Signup),
            (VERIFY_API_PATH, Route::Verify),
            (RESEND_API_PATH, Route::Resend),
        ] {
            let endpoint = InkboxEndpoint {
                state: Rc::clone(&state),
                route,
            };
            let registration = webserver
                .serve_http(path, endpoint)
                .map_err(PluginError::registration)?;
            context.retain(registration);
        }
        Ok(())
    }
}

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
#[derive(Deserialize, Serialize)]
struct StoredConfiguration {
    api_key: String,
    identity_id: String,
    api_base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signup: Option<SignupRecord>,
}

/// Account details of a signup, reported by `GET` without the key.
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
    fn into_inkbox_config(self) -> InkboxConfig {
        InkboxConfig {
            api_key: self.api_key,
            identity_id: self.identity_id,
            api_base: self.api_base,
        }
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

/// Channel state shared by every Inkbox HTTP route.
struct InkboxState<Storage, T: 'static = http_client::Tcp, D: 'static = http_client::Resolver> {
    gateway: Rc<IMessageGateway>,
    http_clients: ClientFactory<'static, T, D>,
    channel_registration: Mutex<NoopRawMutex, Option<MessageChannelRegistration>>,
    /// Whether a channel is registered with the Gateway, readable without the lock.
    configured: Cell<bool>,
    /// Admits one signup, verify, or resend flow at a time, so the Plugin
    /// opens at most one upstream connection for them.
    upstream: Mutex<NoopRawMutex, ()>,
    /// Signup result that failed to be stored or registered, until it is.
    pending_signup: RefCell<Option<PendingSignup>>,
    storage: Storage,
    /// Inkbox service origin used for signup and stored with its result.
    signup_api_base: String,
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

impl<Storage, T, D> InkboxState<Storage, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    /// `GET`: whether a channel is configured and, for a signup, its account.
    async fn configuration_status(&self) -> HttpResponse {
        if !self.configured.get() {
            return static_response(200, br#"{"configured":false}"#);
        }
        let signup = match self.stored_configuration().await {
            Ok(stored) => stored.signup,
            Err(StoredSignupError::Missing) => None,
            Err(StoredSignupError::Storage) => {
                return static_response(500, br#"{"error":"storage"}"#);
            }
        };
        #[derive(Serialize)]
        struct Body {
            configured: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            signup: Option<SignupRecord>,
        }
        match serde_json::to_vec(&Body {
            configured: true,
            signup,
        }) {
            Ok(body) => HttpResponse::new(200, JSON_CONTENT_TYPE, body),
            Err(_) => static_response(500, br#"{"error":"internal"}"#),
        }
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
        let client = InkboxSignup::new(self.http_clients.clone(), self.signup_api_base.as_str());
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
        let failure = match self.apply(config).await {
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
        match InkboxSignup::new(self.http_clients.clone(), config.api_base.as_str())
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
        match InkboxSignup::new(self.http_clients.clone(), config.api_base)
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
        let _registration = self.channel_registration.lock().await;
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

    /// Persists `config` and replaces the registered channel with it, restoring
    /// the previous stored configuration when the Gateway rejects the channel.
    async fn apply(&self, config: StoredConfiguration) -> Result<(), ApplyError> {
        let mut channel_registration = self.channel_registration.lock().await;
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
        let channel: Rc<dyn MessageChannel> = Rc::new(Inkbox::new(
            self.http_clients.clone(),
            config.into_inkbox_config(),
        ));
        channel_registration.take();
        self.configured.set(false);
        match self.gateway.register(channel) {
            Ok(registration) => {
                channel_registration.replace(registration);
                self.configured.set(true);
                log::info!("configured Inkbox gateway provider");
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
                        "failed to roll back rejected Inkbox gateway configuration: {storage_error}"
                    );
                    return Err(ApplyError::Storage);
                }
                log::warn!("rejected Inkbox gateway configuration: {error}");
                Err(ApplyError::Rejected)
            }
        }
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
struct InkboxEndpoint<Storage, T: 'static = http_client::Tcp, D: 'static = http_client::Resolver> {
    state: Rc<InkboxState<Storage, T, D>>,
    route: Route,
}

impl<Storage, T, D> HttpEndpoint for InkboxEndpoint<Storage, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match (request.method(), self.route) {
                (HttpMethod::Post, _) => {}
                (HttpMethod::Get, Route::Config) => {
                    return self.state.configuration_status().await;
                }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use barracuda_imessage_gateway_plugin::{
        ChannelError, ChannelFuture, GatewaySendRequest, IMessageGatewayPlugin, Operation,
        SendMessageRequest, SendReceipt,
    };
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, never_embassy_stack, ScriptStep, ScriptedStack,
    };
    use barracuda_plugin::manager::{PluginDeclaration, PluginManager};
    use barracuda_workflow_plugin::WorkflowPlugin;
    use futures_lite::future::block_on;

    use alloc::format;

    use super::*;

    const API_KEY: &str = "ApiKey_once";
    const IDENTITY_ID: &str = "eeee5555-0000-0000-0000-000000000001";
    const API_BASE: &str = "http://inkbox.test";
    const SIGNUP_RESPONSE: &str = r#"{"email_address":"barracuda-a1b2@inkboxmail.com","agent_handle":"barracuda-a1b2","api_key":"ApiKey_once","organization_id":"org-1","claim_status":"agent_unclaimed","human_email":"person@example.com","message":"Verification email sent"}"#;
    const IDENTITY_RESPONSE: &str =
        r#"{"id":"eeee5555-0000-0000-0000-000000000001","agent_handle":"barracuda-a1b2"}"#;

    /// Type-erased routes and observations of one Inkbox state under test.
    struct Harness {
        network: &'static ScriptedStack,
        gateway: Rc<IMessageGateway>,
        config: Box<dyn HttpEndpoint>,
        signup: Box<dyn HttpEndpoint>,
        verify: Box<dyn HttpEndpoint>,
        resend: Box<dyn HttpEndpoint>,
        stored: Box<dyn Fn() -> Option<Vec<u8>>>,
        entry_status: Box<dyn Fn() -> EntryStatus>,
        /// Another `imessage` channel holding the name while present.
        occupant: RefCell<Option<MessageChannelRegistration>>,
    }

    struct OccupyingChannel;

    impl MessageChannel for OccupyingChannel {
        fn channel(&self) -> &str {
            "imessage"
        }

        fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
            Box::pin(async { Err(ChannelError::unsupported(Operation::SendMessage)) })
        }
    }

    struct Reply {
        status: u16,
        raw: String,
        json: Value,
    }

    impl Harness {
        fn call(endpoint: &dyn HttpEndpoint, method: HttpMethod, body: &str) -> Reply {
            let response = block_on(endpoint.handle(HttpRequest::new(method, body.into())));
            let raw = String::from_utf8(response.body().expect("buffered body").to_vec())
                .expect("UTF-8 body");
            let json = if raw.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&raw).expect("JSON body")
            };
            Reply {
                status: response.status(),
                raw,
                json,
            }
        }

        fn post(endpoint: &dyn HttpEndpoint, body: &str) -> Reply {
            Self::call(endpoint, HttpMethod::Post, body)
        }

        fn configure(&self) {
            let body = r#"{"api_key":"ApiKey_once","identity_id":"eeee5555-0000-0000-0000-000000000001","api_base":"http://inkbox.test"}"#;
            assert_eq!(Self::post(&*self.config, body).status, 204);
        }

        fn get(&self) -> Reply {
            Self::call(&*self.config, HttpMethod::Get, "")
        }

        fn stored(&self) -> Option<StoredConfiguration> {
            (self.stored)().map(|bytes| decode_configuration(&bytes).expect("stored JSON"))
        }

        fn requests(&self) -> Vec<String> {
            self.network.requests()
        }

        fn send_text(&self) -> bool {
            block_on(self.gateway.send(GatewaySendRequest {
                channel: "imessage".into(),
                conversation_id: "conversation-uuid".into(),
                thread_id: None,
                reply_to: None,
                text: "hello".into(),
            }))
            .is_ok()
        }
    }

    struct Probe {
        network: &'static ScriptedStack,
        occupied: bool,
        harness: Rc<RefCell<Option<Harness>>>,
    }

    impl PluginDeclaration for Probe {
        const ID: &'static str = "inkbox-probe";
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
            let occupant = if self.occupied {
                let channel: Rc<dyn MessageChannel> = Rc::new(OccupyingChannel);
                Some(
                    gateway
                        .register(channel)
                        .map_err(PluginError::registration)?,
                )
            } else {
                None
            };
            let storage = context.storage().clone();
            let state = Rc::new(InkboxState {
                gateway: Rc::clone(&gateway),
                http_clients: ClientFactory::from_network(self.network, self.network),
                channel_registration: Mutex::new(None),
                configured: Cell::new(false),
                upstream: Mutex::new(()),
                pending_signup: RefCell::new(None),
                storage: storage.clone(),
                signup_api_base: API_BASE.into(),
            });
            let status = Rc::clone(&state);
            let endpoint = |route| -> Box<dyn HttpEndpoint> {
                Box::new(InkboxEndpoint {
                    state: Rc::clone(&state),
                    route,
                })
            };
            self.harness.replace(Some(Harness {
                network: self.network,
                gateway,
                config: endpoint(Route::Config),
                signup: endpoint(Route::Signup),
                verify: endpoint(Route::Verify),
                resend: endpoint(Route::Resend),
                stored: Box::new(move || {
                    block_on(storage.get_bytes(CONFIGURATION_STORAGE_KEY)).expect("read storage")
                }),
                entry_status: Box::new(move || EntryStatus::configured(status.configured.get())),
                occupant: RefCell::new(occupant),
            }));
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

    /// Runs `scenario` against Inkbox routes whose upstream replies are `steps`.
    fn with_harness(steps: impl IntoIterator<Item = ScriptStep>, scenario: impl FnOnce(&Harness)) {
        with_gateway(steps, false, scenario);
    }

    /// As [`with_harness`], with the `imessage` channel taken while `occupied`.
    fn with_gateway(
        steps: impl IntoIterator<Item = ScriptStep>,
        occupied: bool,
        scenario: impl FnOnce(&Harness),
    ) {
        block_on(install_global_memory_vfs()).expect("install test VFS");
        let partition = block_on(memory_partition(64 * 1024)).expect("create test database region");
        let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
        manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
        let mut context = plugin_context();
        manager
            .register(WorkflowPlugin::new(&mut context))
            .expect("register Workflow Plugin");
        manager
            .register(IMessageGatewayPlugin::new(&mut context))
            .expect("register IMessage Gateway Plugin");
        let harness = Rc::new(RefCell::new(None));
        manager
            .register(Probe {
                network: Box::leak(Box::new(ScriptedStack::new(steps))),
                occupied,
                harness: Rc::clone(&harness),
            })
            .expect("register Inkbox probe");
        let harness = harness.borrow_mut().take().expect("probe registered");
        scenario(&harness);
    }

    fn body_of(request: &str) -> Value {
        let (_, body) = request.split_once("\r\n\r\n").expect("request body");
        serde_json::from_str(body).expect("request JSON")
    }

    fn request_line(request: &str) -> &str {
        request.split("\r\n").next().expect("request line")
    }

    #[test]
    fn stored_configuration_round_trips_every_field() -> Result<(), serde_json::Error> {
        let config = StoredConfiguration {
            api_key: "secret".into(),
            identity_id: "identity".into(),
            api_base: "https://inkbox.example".into(),
            signup: Some(SignupRecord {
                human_email: "person@example.com".into(),
                email_address: "agent@inkboxmail.com".into(),
                claim_status: "agent_unclaimed".into(),
            }),
        };

        let bytes = encode_configuration(&config)?;
        let restored = decode_configuration(&bytes)?;

        assert_eq!(restored.api_key, config.api_key);
        assert_eq!(restored.identity_id, config.identity_id);
        assert_eq!(restored.api_base, config.api_base);
        let signup = restored.signup.expect("signup record");
        assert_eq!(signup.human_email, "person@example.com");
        assert_eq!(signup.email_address, "agent@inkboxmail.com");
        assert_eq!(signup.claim_status, "agent_unclaimed");
        Ok(())
    }

    #[test]
    fn configuration_stored_before_signup_records_still_decodes() -> Result<(), serde_json::Error> {
        let restored = decode_configuration(
            br#"{"api_key":"secret","identity_id":"identity","api_base":"https://inkbox.example"}"#,
        )?;
        assert_eq!(restored.api_key, "secret");
        assert!(restored.signup.is_none());
        Ok(())
    }

    #[test]
    fn signup_stores_the_resolved_identity_and_registers_the_channel() {
        with_harness(
            [
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(200, IDENTITY_RESPONSE),
                ScriptStep::json(200, r#"{"message":{"id":"message-uuid"}}"#),
            ],
            |harness| {
                let reply = Harness::post(&*harness.signup, r#"{"email":" person@example.com "}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(
                    reply.json,
                    json!({
                        "email_address": "barracuda-a1b2@inkboxmail.com",
                        "claim_status": "agent_unclaimed",
                    })
                );
                assert!(!reply.raw.contains(API_KEY));
                let stored = harness.stored().expect("configuration stored");
                assert_eq!(stored.api_key, API_KEY);
                assert_eq!(stored.identity_id, IDENTITY_ID);
                assert_eq!(stored.api_base, API_BASE);
                let get = harness.get();
                assert_eq!(get.status, 200);
                assert_eq!(
                    get.json,
                    json!({
                        "configured": true,
                        "signup": {
                            "human_email": "person@example.com",
                            "email_address": "barracuda-a1b2@inkboxmail.com",
                            "claim_status": "agent_unclaimed",
                        },
                    })
                );
                assert!(!get.raw.contains(API_KEY));
                assert_eq!((harness.entry_status)(), EntryStatus::configured(true));

                assert!(harness.send_text());
                let requests = harness.requests();
                assert_eq!(
                    request_line(&requests[0]),
                    "POST /api/v1/agent-signup HTTP/1.1"
                );
                assert_eq!(
                    body_of(&requests[0]),
                    json!({
                        "human_email": "person@example.com",
                        "note_to_human": SIGNUP_NOTE_TO_HUMAN,
                        "display_name": "Barracuda",
                        "harness": "barracuda",
                    })
                );
                assert!(!requests[0].contains("X-API-Key"));
                assert_eq!(
                    request_line(&requests[1]),
                    "GET /api/v1/identities/barracuda-a1b2 HTTP/1.1"
                );
                assert!(requests[1].contains("X-API-Key: ApiKey_once\r\n"));
                assert_eq!(
                    request_line(&requests[2]),
                    "POST /api/v1/imessage/messages?agent_identity_id=eeee5555-0000-0000-0000-000000000001 HTTP/1.1"
                );
                assert!(requests[2].contains("X-API-Key: ApiKey_once\r\n"));
            },
        );
    }

    #[test]
    fn signup_validation_failure_maps_to_invalid_request_and_stores_nothing() {
        with_harness(
            [ScriptStep::json(
                422,
                r#"{"detail":[{"loc":["body","human_email"],"msg":"value is not a valid email address","type":"value_error"}]}"#,
            )],
            |harness| {
                let reply = Harness::post(&*harness.signup, r#"{"email":"not-an-email"}"#);

                assert_eq!(reply.status, 400);
                assert_eq!(
                    reply.json,
                    json!({
                        "error": "invalid_request",
                        "message": "value is not a valid email address",
                    })
                );
                assert!(harness.stored().is_none());
                assert!(!harness.send_text());
            },
        );
    }

    #[test]
    fn signup_rejects_a_missing_email_without_calling_inkbox() {
        with_harness([], |harness| {
            assert_eq!(Harness::post(&*harness.signup, "{}").status, 400);
            assert_eq!(
                Harness::post(&*harness.signup, r#"{"email":"  "}"#).status,
                400
            );
            assert!(harness.requests().is_empty());
        });
    }

    fn signup_count(harness: &Harness) -> usize {
        harness
            .requests()
            .iter()
            .filter(|request| request_line(request) == "POST /api/v1/agent-signup HTTP/1.1")
            .count()
    }

    #[test]
    fn unresolved_identity_keeps_the_signup_and_a_retry_resumes_it() {
        with_harness(
            [
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(404, r#"{"detail":"Identity not found"}"#),
                ScriptStep::json(200, IDENTITY_RESPONSE),
            ],
            |harness| {
                let reply = Harness::post(&*harness.signup, r#"{"email":"person@example.com"}"#);

                assert_eq!(reply.status, 502);
                assert_eq!(
                    reply.json,
                    json!({
                        "error": "upstream_unavailable",
                        "message": "Identity not found",
                        "retry": true,
                    })
                );
                assert!(!reply.raw.contains(API_KEY));
                assert!(harness.stored().is_none());
                assert_eq!(harness.get().json, json!({"configured": false}));

                let reply = Harness::post(&*harness.signup, r#"{"email":" person@example.com "}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(
                    reply.json,
                    json!({
                        "email_address": "barracuda-a1b2@inkboxmail.com",
                        "claim_status": "agent_unclaimed",
                    })
                );
                assert_eq!(signup_count(harness), 1);
                let requests = harness.requests();
                assert_eq!(requests.len(), 3);
                assert_eq!(
                    request_line(&requests[2]),
                    "GET /api/v1/identities/barracuda-a1b2 HTTP/1.1"
                );
                let stored = harness.stored().expect("configuration stored");
                assert_eq!(stored.api_key, API_KEY);
                assert_eq!(stored.identity_id, IDENTITY_ID);
            },
        );
    }

    #[test]
    fn rejected_registration_keeps_the_signup_and_a_retry_resumes_it() {
        with_gateway(
            [
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(200, IDENTITY_RESPONSE),
            ],
            true,
            |harness| {
                let reply = Harness::post(&*harness.signup, r#"{"email":"person@example.com"}"#);

                assert_eq!(reply.status, 422);
                assert_eq!(
                    reply.json,
                    json!({"error": "registration_failed", "retry": true})
                );
                assert!(harness.stored().is_none());
                assert_eq!((harness.entry_status)(), EntryStatus::configured(false));

                harness.occupant.replace(None);
                let reply = Harness::post(&*harness.signup, r#"{"email":"person@example.com"}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(reply.json["email_address"], "barracuda-a1b2@inkboxmail.com");
                // Neither signup nor the identity lookup is repeated.
                assert_eq!(harness.requests().len(), 2);
                assert_eq!(signup_count(harness), 1);
                assert_eq!(
                    harness.stored().expect("configuration stored").api_key,
                    API_KEY
                );
                assert_eq!((harness.entry_status)(), EntryStatus::configured(true));
            },
        );
    }

    #[test]
    fn storage_failure_keeps_the_signup_and_a_retry_resumes_it() {
        // A key longer than one storage value makes every store fail.
        let api_key = format!("ApiKey_{}", "k".repeat(1100));
        let signup = SIGNUP_RESPONSE.replace(API_KEY, &api_key);
        with_harness(
            [
                ScriptStep::json(200, &signup),
                ScriptStep::json(200, IDENTITY_RESPONSE),
            ],
            |harness| {
                for _ in 0..2 {
                    let reply =
                        Harness::post(&*harness.signup, r#"{"email":"person@example.com"}"#);

                    assert_eq!(reply.status, 500);
                    assert_eq!(reply.json, json!({"error": "storage", "retry": true}));
                    assert!(!reply.raw.contains("ApiKey_"));
                }
                assert_eq!(signup_count(harness), 1);
                assert_eq!(harness.requests().len(), 2);
                assert!(harness.stored().is_none());
            },
        );
    }

    #[test]
    fn a_kept_signup_is_not_reused_for_another_email() {
        with_gateway(
            [
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(200, IDENTITY_RESPONSE),
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(200, IDENTITY_RESPONSE),
            ],
            true,
            |harness| {
                let reply = Harness::post(&*harness.signup, r#"{"email":"person@example.com"}"#);
                assert_eq!(reply.status, 422);

                harness.occupant.replace(None);
                let reply = Harness::post(&*harness.signup, r#"{"email":"other@example.com"}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(signup_count(harness), 2);
            },
        );
    }

    #[test]
    fn manual_configuration_rejected_by_the_gateway_is_registration_failed() {
        with_gateway([], true, |harness| {
            let body = r#"{"api_key":"ApiKey_once","identity_id":"eeee5555-0000-0000-0000-000000000001","api_base":"http://inkbox.test"}"#;
            let reply = Harness::post(&*harness.config, body);

            assert_eq!(reply.status, 422);
            assert_eq!(reply.json, json!({"error": "registration_failed"}));
            assert!(harness.stored().is_none());
            assert_eq!(harness.get().json, json!({"configured": false}));
        });
    }

    #[test]
    fn get_reports_configuration_without_secrets() {
        with_harness([], |harness| {
            let reply = harness.get();
            assert_eq!(reply.status, 200);
            assert_eq!(reply.json, json!({"configured": false}));
            assert_eq!((harness.entry_status)(), EntryStatus::configured(false));

            harness.configure();

            let reply = harness.get();
            assert_eq!(reply.status, 200);
            assert_eq!(reply.raw, r#"{"configured":true}"#);
            assert_eq!((harness.entry_status)(), EntryStatus::configured(true));
        });
    }

    #[test]
    fn verify_submits_the_code_with_the_stored_key() {
        with_harness(
            [ScriptStep::json(
                200,
                r#"{"claim_status":"agent_claimed","organization_id":"org-2","message":"ok"}"#,
            )],
            |harness| {
                harness.configure();
                let reply = Harness::post(&*harness.verify, r#"{"code":"483921"}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(reply.json, json!({"claim_status": "agent_claimed"}));
                // A manual configuration has no signup record to update.
                assert_eq!(harness.get().json, json!({"configured": true}));
                let requests = harness.requests();
                assert_eq!(
                    request_line(&requests[0]),
                    "POST /api/v1/agent-signup/verify HTTP/1.1"
                );
                assert!(requests[0].contains("X-API-Key: ApiKey_once\r\n"));
                assert_eq!(
                    body_of(&requests[0]),
                    json!({"verification_code": "483921"})
                );
            },
        );
    }

    #[test]
    fn verify_updates_the_stored_claim_status_of_a_signup() {
        with_harness(
            [
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(200, IDENTITY_RESPONSE),
                ScriptStep::json(
                    200,
                    r#"{"claim_status":"agent_claimed","organization_id":"org-2","message":"ok"}"#,
                ),
            ],
            |harness| {
                let reply = Harness::post(&*harness.signup, r#"{"email":"person@example.com"}"#);
                assert_eq!(reply.status, 200);

                let reply = Harness::post(&*harness.verify, r#"{"code":"483921"}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(
                    harness.get().json,
                    json!({
                        "configured": true,
                        "signup": {
                            "human_email": "person@example.com",
                            "email_address": "barracuda-a1b2@inkboxmail.com",
                            "claim_status": "agent_claimed",
                        },
                    })
                );
                assert_eq!(
                    harness.stored().expect("configuration stored").api_key,
                    API_KEY
                );
            },
        );
    }

    #[test]
    fn wrong_code_is_verification_failed_with_the_upstream_message() {
        with_harness(
            [ScriptStep::json(
                422,
                r#"{"detail":"Invalid verification code"}"#,
            )],
            |harness| {
                harness.configure();
                let reply = Harness::post(&*harness.verify, r#"{"code":"000000"}"#);

                assert_eq!(reply.status, 422);
                assert_eq!(
                    reply.json,
                    json!({"error": "verification_failed", "message": "Invalid verification code"})
                );
            },
        );
    }

    #[test]
    fn verify_rejects_a_malformed_code_without_calling_inkbox() {
        with_harness([], |harness| {
            harness.configure();
            for body in [
                r#"{"code":"12345"}"#,
                r#"{"code":"12345a"}"#,
                r#"{"otp":"123456"}"#,
            ] {
                assert_eq!(Harness::post(&*harness.verify, body).status, 400);
            }
            assert!(harness.requests().is_empty());
        });
    }

    #[test]
    fn verify_and_resend_without_a_signup_conflict() {
        with_harness([], |harness| {
            let verify = Harness::post(&*harness.verify, r#"{"code":"123456"}"#);
            let resend = Harness::post(&*harness.resend, "");

            assert_eq!(verify.status, 409);
            assert_eq!(verify.json["error"], "conflict");
            assert_eq!(resend.status, 409);
            assert_eq!(resend.json["error"], "conflict");
            assert!(harness.requests().is_empty());
        });
    }

    #[test]
    fn resend_asks_inkbox_with_the_stored_key() {
        with_harness(
            [
                ScriptStep::json(
                    200,
                    r#"{"claim_status":"agent_unclaimed","organization_id":"org-1","message":"sent"}"#,
                ),
                ScriptStep::json(429, r#"{"detail":"Please wait before resending"}"#),
            ],
            |harness| {
                harness.configure();
                let sent = Harness::post(&*harness.resend, "");
                let limited = Harness::post(&*harness.resend, "");

                assert_eq!(sent.status, 204);
                assert!(sent.raw.is_empty());
                assert_eq!(
                    limited.json,
                    json!({"error": "verification_failed", "message": "Please wait before resending"})
                );
                let requests = harness.requests();
                assert_eq!(
                    request_line(&requests[0]),
                    "POST /api/v1/agent-signup/resend-verification HTTP/1.1"
                );
                assert!(requests[0].contains("X-API-Key: ApiKey_once\r\n"));
            },
        );
    }

    #[test]
    fn unreachable_inkbox_is_upstream_unavailable() {
        with_harness(
            [ScriptStep::response(
                502,
                "text/html",
                b"<html>bad gateway</html>",
                usize::MAX,
            )],
            |harness| {
                let reply = Harness::post(&*harness.signup, r#"{"email":"person@example.com"}"#);

                assert_eq!(reply.status, 502);
                assert_eq!(reply.json["error"], "upstream_unavailable");
            },
        );
    }

    #[test]
    fn every_route_rejects_other_methods() {
        with_harness([], |harness| {
            for endpoint in [
                &harness.config,
                &harness.signup,
                &harness.verify,
                &harness.resend,
            ] {
                for method in [HttpMethod::Put, HttpMethod::Delete, HttpMethod::Patch] {
                    let reply = Harness::call(&**endpoint, method, "");
                    assert_eq!(reply.status, 405);
                    assert_eq!(reply.json, json!({"error": "method_not_allowed"}));
                }
            }
            // Only the configuration path answers GET.
            for endpoint in [&harness.signup, &harness.verify, &harness.resend] {
                let reply = Harness::call(&**endpoint, HttpMethod::Get, "");
                assert_eq!(reply.status, 405);
            }
        });
    }
}

#![allow(clippy::expect_used, clippy::panic)]

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::Any;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;

use barracuda_captive_portal_plugin::{EntryState, EntryStatus, WebText};
use barracuda_imessage_gateway_channel::{channel_entry_status, ChannelMode, ReceiveState};
use barracuda_imessage_gateway_plugin::{
    ChannelError, ChannelFuture, IMessageGateway, IMessageGatewayPlugin, MessageChannel, Operation,
    SendMessageRequest, SendReceipt,
};
use barracuda_platform_test::{
    install_global_memory_vfs, memory_partition, never_embassy_stack, ScriptStep, ScriptedStack,
};
use barracuda_plugin::api::{PluginContext, SharedEntropy};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginError, PluginManager, PluginRegisterContext, PluginResult,
    PluginStorage,
};
use barracuda_webserver_plugin::{HttpEndpoint, HttpMethod, HttpRequest};
use barracuda_workflow_plugin::WorkflowPlugin;
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant};
use futures_lite::future::{block_on, yield_now};
use http_client::{ClientFactory, ReceiveSlots};
use serde_json::{json, Value};

use super::{LoginEndpoint, LoginRuntime, LoginTiming};
use crate::receive::PollTiming;
use crate::{
    decode_configuration, ChannelConfiguration, ConfigEndpoint, CONFIGURATION_STORAGE_KEY,
};

const API_BASE: &str = "http://wechat.test";
const QR_URL_1: &str = "https://liteapp.weixin.qq.com/q/one?qrcode=qr-1&bot_type=3";
const QR_URL_2: &str = "https://liteapp.weixin.qq.com/q/two?qrcode=qr-2&bot_type=3";
const BOT_TOKEN: &str = "bot-secret-token";
const TEST_TIMING: LoginTiming = LoginTiming {
    session: Duration::from_secs(60),
    min_poll_interval: Duration::from_ticks(0),
};

type StoredReader = Box<dyn Fn() -> Pin<Box<dyn Future<Output = Option<Vec<u8>>>>>>;
type StatusReader = Box<dyn Fn() -> EntryStatus>;

/// Endpoints and runtime built against Plugin storage and a real Gateway.
struct Parts {
    login: Rc<dyn HttpEndpoint>,
    config: Rc<dyn HttpEndpoint>,
    runtime: LoginRuntime,
    gateway: Rc<IMessageGateway>,
    stored: StoredReader,
    entry_status: StatusReader,
}

struct Probe {
    network: &'static ScriptedStack,
    timing: LoginTiming,
    occupy_channel: bool,
    parts: Rc<RefCell<Option<Parts>>>,
}

impl PluginDeclaration for Probe {
    const ID: &'static str = "wechat-login-probe";
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
        if self.occupy_channel {
            context.retain(
                gateway
                    .register(Rc::new(OtherWechatChannel))
                    .map_err(PluginError::registration)?,
            );
        }
        let configuration = Rc::new(block_on(ChannelConfiguration::load(
            Rc::clone(&gateway),
            ClientFactory::from_network(self.network, self.network),
            context.storage().clone(),
            ReceiveSlots::<ScriptedStack, ScriptedStack>::unavailable(),
            SharedEntropy::unavailable(),
            PollTiming::DEVICE,
        ))?);
        let (login, runtime) =
            LoginEndpoint::new(Rc::clone(&configuration), API_BASE.into(), self.timing);
        let watch = login.watch();
        let status_configuration = Rc::clone(&configuration);
        let entry_status: StatusReader =
            Box::new(move || crate::entry_status(&status_configuration, &watch));
        let storage = context.storage().clone();
        let stored: StoredReader = Box::new(move || {
            let storage = storage.clone();
            Box::pin(async move {
                storage
                    .get_bytes(CONFIGURATION_STORAGE_KEY)
                    .await
                    .expect("read stored configuration")
            })
        });
        self.parts.replace(Some(Parts {
            login: Rc::new(login),
            config: Rc::new(ConfigEndpoint { configuration }),
            runtime,
            gateway,
            stored,
            entry_status,
        }));
        Ok(())
    }
}

/// A second `wechat` channel, used to occupy the name or to probe registration.
struct OtherWechatChannel;

impl MessageChannel for OtherWechatChannel {
    fn channel(&self) -> &str {
        "wechat"
    }

    fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::SendMessage)) })
    }
}

struct Harness {
    login: Rc<dyn HttpEndpoint>,
    config: Rc<dyn HttpEndpoint>,
    gateway: Rc<IMessageGateway>,
    stored: StoredReader,
    entry_status: StatusReader,
    network: &'static ScriptedStack,
    _manager: Box<dyn Any>,
}

impl Harness {
    async fn call(&self, method: HttpMethod, body: &[u8]) -> (u16, Value) {
        call(self.login.as_ref(), method, body).await
    }

    async fn status(&self) -> Value {
        let (code, body) = self.call(HttpMethod::Get, b"").await;
        assert_eq!(code, 200);
        body
    }

    /// Lets the runtime run until GET reports `expected`.
    async fn wait_for_status(&self, expected: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = self.status().await;
            if status["status"] == expected {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "status stayed {status} instead of {expected}"
            );
            yield_now().await;
        }
    }

    /// Lets the runtime run until the mock iLink has seen `count` requests.
    async fn wait_for_requests(&self, count: usize) -> Vec<String> {
        for _ in 0..10_000 {
            let requests = self.request_paths();
            if requests.len() >= count {
                return requests;
            }
            yield_now().await;
        }
        panic!(
            "iLink saw {:?} instead of {count} requests",
            self.request_paths()
        );
    }

    async fn settle(&self) {
        for _ in 0..1_000 {
            yield_now().await;
        }
    }

    fn request_paths(&self) -> Vec<String> {
        self.network
            .requests()
            .iter()
            .map(|request| {
                request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or_default()
                    .into()
            })
            .collect()
    }

    fn channel_registered(&self) -> bool {
        self.gateway.register(Rc::new(OtherWechatChannel)).is_err()
    }
}

async fn call(endpoint: &dyn HttpEndpoint, method: HttpMethod, body: &[u8]) -> (u16, Value) {
    let response = endpoint
        .handle(HttpRequest::new(method, body.to_vec()))
        .await;
    let body = response.body().expect("buffered response");
    let json = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).expect("JSON response")
    };
    (response.status(), json)
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

/// Builds the login service over mock iLink `steps` and runs `scenario`
/// beside the login runtime, as the Plugin's task would.
fn scenario<F, Fut>(steps: Vec<ScriptStep>, timing: LoginTiming, occupy_channel: bool, test: F)
where
    F: FnOnce(Rc<Harness>) -> Fut,
    Fut: Future<Output = ()>,
{
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("create test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let mut context = plugin_context();
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new(steps)));
    let parts = Rc::new(RefCell::new(None));
    manager
        .register(WorkflowPlugin::new(&mut context))
        .expect("register Workflow Plugin");
    manager
        .register(IMessageGatewayPlugin::new(&mut context))
        .expect("register IMessage Gateway Plugin");
    manager
        .register(Probe {
            network,
            timing,
            occupy_channel,
            parts: Rc::clone(&parts),
        })
        .expect("register login probe");
    let Parts {
        login,
        config,
        runtime,
        gateway,
        stored,
        entry_status,
    } = parts.take().expect("probe built the login service");
    let harness = Rc::new(Harness {
        login,
        config,
        gateway,
        stored,
        entry_status,
        network,
        _manager: Box::new(manager),
    });
    block_on(async move {
        match select(runtime, test(harness)).await {
            Either::First(()) => panic!("login runtime ended"),
            Either::Second(()) => {}
        }
    });
}

fn qrcode(id: &str, url: &str) -> ScriptStep {
    ScriptStep::json(
        200,
        &json!({ "qrcode": id, "qrcode_img_content": url, "ret": 0 }).to_string(),
    )
}

fn status(status: &str) -> ScriptStep {
    ScriptStep::json(200, &json!({ "ret": 0, "status": status }).to_string())
}

fn confirmed() -> ScriptStep {
    ScriptStep::json(
        200,
        &json!({
            "ret": 0,
            "status": "confirmed",
            "bot_token": BOT_TOKEN,
            "ilink_bot_id": "bot@im.bot",
            "baseurl": "http://ilink.example",
        })
        .to_string(),
    )
}

fn long_poll() -> ScriptStep {
    ScriptStep::pending_after_headers(200, "application/json")
}

/// Portal status of a linked channel when the test pool has no receive slot.
fn linked_without_slot() -> EntryStatus {
    channel_entry_status(true, ChannelMode::SendReceive, &ReceiveState::NoSlot)
}

#[test]
fn get_before_any_session_is_idle() {
    scenario(Vec::new(), TEST_TIMING, false, |harness| async move {
        assert_eq!(
            harness.status().await,
            json!({ "status": "idle", "configured": false })
        );
        assert!(harness.request_paths().is_empty());
    });
}

#[test]
fn start_returns_the_qr_url_and_reports_wait() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), long_poll()]);
    scenario(steps, TEST_TIMING, false, |harness| async move {
        let (code, body) = harness.call(HttpMethod::Post, b"").await;

        assert_eq!(code, 200);
        assert_eq!(body, json!({ "url": QR_URL_1, "expires_in": 480 }));
        assert_eq!(
            harness.wait_for_status("wait").await,
            json!({ "status": "wait", "configured": false })
        );
        assert_eq!(
            harness.wait_for_requests(2).await,
            [
                "/ilink/bot/get_bot_qrcode?bot_type=3",
                "/ilink/bot/get_qrcode_status?qrcode=qr-1",
            ]
        );
    });
}

#[test]
fn scaned_is_reported_as_scanned() {
    let steps = Vec::from([
        qrcode("qr-1", QR_URL_1),
        status("wait"),
        status("scaned"),
        long_poll(),
    ]);
    scenario(steps, TEST_TIMING, false, |harness| async move {
        let (code, _body) = harness.call(HttpMethod::Post, b"{}").await;

        assert_eq!(code, 200);
        assert_eq!(
            harness.wait_for_status("scanned").await,
            json!({ "status": "scanned", "configured": false })
        );
    });
}

#[test]
fn confirmed_login_stores_and_registers_the_channel() {
    let steps = Vec::from([
        qrcode("qr-1", QR_URL_1),
        status("wait"),
        status("scaned"),
        confirmed(),
    ]);
    scenario(steps, TEST_TIMING, false, |harness| async move {
        assert!(!harness.channel_registered());
        let (code, body) = harness.call(HttpMethod::Post, b"").await;
        assert_eq!(code, 200);
        assert!(!body.to_string().contains(BOT_TOKEN));

        let status = harness.wait_for_status("confirmed").await;

        assert_eq!(status, json!({ "status": "confirmed", "configured": true }));
        assert!(!status.to_string().contains(BOT_TOKEN));
        assert!(harness.channel_registered());
        let stored = (harness.stored)().await.expect("configuration stored");
        let config = decode_configuration(&stored).expect("stored configuration decodes");
        assert_eq!(config.token, BOT_TOKEN);
        assert_eq!(config.api_base, "http://ilink.example");
        assert_eq!(config.app_id, "bot");
        assert_eq!(config.client_version, "131329");
        assert_eq!(config.route_tag, None);
        assert_eq!(config.x_wechat_uin, "MA==");
        harness.settle().await;
        assert_eq!(harness.request_paths().len(), 4, "the session ended");
    });
}

#[test]
fn expired_qr_code_ends_the_session() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), status("expired")]);
    scenario(steps, TEST_TIMING, false, |harness| async move {
        let (code, _body) = harness.call(HttpMethod::Post, b"").await;
        assert_eq!(code, 200);

        assert_eq!(
            harness.wait_for_status("expired").await,
            json!({ "status": "expired", "configured": false })
        );
        harness.settle().await;
        assert_eq!(harness.request_paths().len(), 2, "the session ended");
        assert!((harness.stored)().await.is_none());
    });
}

#[test]
fn session_deadline_reports_expired() {
    let timing = LoginTiming {
        session: Duration::from_millis(50),
        ..TEST_TIMING
    };
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), long_poll()]);
    scenario(steps, timing, false, |harness| async move {
        let (code, _body) = harness.call(HttpMethod::Post, b"").await;
        assert_eq!(code, 200);

        assert_eq!(
            harness.wait_for_status("expired").await["status"],
            "expired"
        );
    });
}

#[test]
fn delete_cancels_the_session() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), long_poll()]);
    scenario(steps, TEST_TIMING, false, |harness| async move {
        let (code, _body) = harness.call(HttpMethod::Post, b"").await;
        assert_eq!(code, 200);
        harness.wait_for_requests(2).await;

        let (code, body) = harness.call(HttpMethod::Delete, b"").await;

        assert_eq!(code, 204);
        assert_eq!(body, Value::Null);
        assert_eq!(
            harness.status().await,
            json!({ "status": "idle", "configured": false })
        );
        harness.settle().await;
        assert_eq!(harness.status().await["status"], "idle");
        assert_eq!(harness.request_paths().len(), 2);
    });
}

#[test]
fn new_start_replaces_the_running_session() {
    let steps = Vec::from([
        qrcode("qr-1", QR_URL_1),
        long_poll(),
        qrcode("qr-2", QR_URL_2),
        long_poll(),
    ]);
    scenario(steps, TEST_TIMING, false, |harness| async move {
        let (_code, first) = harness.call(HttpMethod::Post, b"").await;
        assert_eq!(first["url"], QR_URL_1);
        harness.wait_for_requests(2).await;

        let (code, second) = harness.call(HttpMethod::Post, b"").await;

        assert_eq!(code, 200);
        assert_eq!(second["url"], QR_URL_2);
        let requests = harness.wait_for_requests(4).await;
        assert_eq!(
            requests.get(3).map(String::as_str),
            Some("/ilink/bot/get_qrcode_status?qrcode=qr-2")
        );
        assert_eq!(harness.status().await["status"], "wait");
    });
}

#[test]
fn unreachable_upstream_returns_502() {
    scenario(Vec::new(), TEST_TIMING, false, |harness| async move {
        let (code, body) = harness.call(HttpMethod::Post, b"").await;

        assert_eq!(code, 502);
        assert_eq!(body["error"], "upstream_unavailable");
        assert!(body["message"].is_string());
        assert_eq!(harness.status().await["status"], "idle");
    });
}

#[test]
fn upstream_error_code_and_message_pass_through() {
    let steps = Vec::from([ScriptStep::json(
        200,
        r#"{"ret":-14,"errmsg":"session timeout"}"#,
    )]);
    scenario(steps, TEST_TIMING, false, |harness| async move {
        let (code, body) = harness.call(HttpMethod::Post, b"").await;

        assert_eq!(code, 502);
        assert_eq!(
            body,
            json!({
                "error": "upstream_unavailable",
                "message": "session timeout",
                "code": "-14",
            })
        );
    });
}

#[test]
fn failed_status_poll_reports_failed() {
    let steps = Vec::from([
        qrcode("qr-1", QR_URL_1),
        ScriptStep::json(200, r#"{"ret":1,"errmsg":"bad qrcode"}"#),
    ]);
    scenario(steps, TEST_TIMING, false, |harness| async move {
        harness.call(HttpMethod::Post, b"").await;

        let status = harness.wait_for_status("failed").await;

        assert_eq!(status["configured"], false);
        assert!(status["message"]
            .as_str()
            .is_some_and(|message| message.contains("bad qrcode")));
    });
}

#[test]
fn rejected_registration_reports_failed() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), confirmed()]);
    scenario(steps, TEST_TIMING, true, |harness| async move {
        harness.call(HttpMethod::Post, b"").await;

        let status = harness.wait_for_status("failed").await;

        assert_eq!(status["configured"], false);
        assert!(status["message"].is_string());
        assert!(!status.to_string().contains(BOT_TOKEN));
        assert!((harness.stored)().await.is_none());
    });
}

#[test]
fn configured_reflects_the_registered_channel() {
    scenario(Vec::new(), TEST_TIMING, false, |harness| async move {
        let (code, _body) = call(
            harness.config.as_ref(),
            HttpMethod::Post,
            br#"{"token":"manual-token"}"#,
        )
        .await;
        assert_eq!(code, 204);

        assert_eq!(
            harness.status().await,
            json!({ "status": "idle", "configured": true })
        );
    });
}

#[test]
fn wrong_methods_return_405() {
    scenario(Vec::new(), TEST_TIMING, false, |harness| async move {
        for method in [HttpMethod::Put, HttpMethod::Patch, HttpMethod::Options] {
            let (code, body) = harness.call(method, b"").await;
            assert_eq!(code, 405);
            assert_eq!(body, json!({ "error": "method_not_allowed" }));
        }
        for method in [HttpMethod::Put, HttpMethod::Delete] {
            let (code, body) = call(harness.config.as_ref(), method, b"").await;
            assert_eq!(code, 405);
            assert_eq!(body, json!({ "error": "method_not_allowed" }));
        }
    });
}

#[test]
fn config_get_reports_only_whether_a_channel_is_configured() {
    scenario(Vec::new(), TEST_TIMING, false, |harness| async move {
        let (code, body) = call(harness.config.as_ref(), HttpMethod::Get, b"").await;
        assert_eq!(code, 200);
        assert_eq!(body["configured"], false);
        assert_eq!((harness.entry_status)(), EntryStatus::configured(false));

        let (code, _body) = call(
            harness.config.as_ref(),
            HttpMethod::Post,
            br#"{"token":"manual-token"}"#,
        )
        .await;
        assert_eq!(code, 204);

        let (code, body) = call(harness.config.as_ref(), HttpMethod::Get, b"").await;
        assert_eq!(code, 200);
        assert_eq!(body["configured"], true);
        assert_eq!(body["mode"], "send_receive");
        assert!(!body.to_string().contains("manual-token"));
        assert_eq!((harness.entry_status)(), linked_without_slot());
    });
}

#[test]
fn config_rejected_by_the_gateway_is_registration_failed() {
    scenario(Vec::new(), TEST_TIMING, true, |harness| async move {
        let (code, body) = call(
            harness.config.as_ref(),
            HttpMethod::Post,
            br#"{"token":"manual-token"}"#,
        )
        .await;

        assert_eq!(code, 422);
        assert_eq!(body, json!({ "error": "registration_failed" }));
        assert!((harness.stored)().await.is_none());
        let (_code, body) = call(harness.config.as_ref(), HttpMethod::Get, b"").await;
        assert_eq!(body["configured"], false);
        assert_eq!((harness.entry_status)(), EntryStatus::configured(false));
    });
}

#[test]
fn entry_status_waits_for_the_scan_until_the_login_is_confirmed() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), status("scaned"), long_poll()]);
    let waiting = EntryStatus::new(
        EntryState::Attention,
        WebText {
            zh: "等待扫码",
            en: "Waiting for scan",
        },
    );
    scenario(steps, TEST_TIMING, false, |harness| async move {
        assert_eq!((harness.entry_status)(), EntryStatus::configured(false));
        let (code, _body) = harness.call(HttpMethod::Post, b"").await;
        assert_eq!(code, 200);
        // The session reports `wait` before the start request is answered.
        assert_eq!((harness.entry_status)(), waiting);
        harness.wait_for_status("scanned").await;
        assert_eq!((harness.entry_status)(), waiting);

        harness.call(HttpMethod::Delete, b"").await;
        assert_eq!((harness.entry_status)(), EntryStatus::configured(false));
    });
}

#[test]
fn entry_status_is_ready_after_a_confirmed_login() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), confirmed()]);
    scenario(steps, TEST_TIMING, false, |harness| async move {
        harness.call(HttpMethod::Post, b"").await;
        harness.wait_for_status("confirmed").await;
        assert_eq!((harness.entry_status)(), linked_without_slot());
    });
}

#[test]
fn start_rejects_a_body_with_fields() {
    scenario(Vec::new(), TEST_TIMING, false, |harness| async move {
        let (code, body) = harness.call(HttpMethod::Post, br#"{"token":"x"}"#).await;

        assert_eq!(code, 400);
        assert_eq!(body, json!({ "error": "invalid_request" }));
        assert!(harness.request_paths().is_empty());
    });
}

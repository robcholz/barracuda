//! The QR login against a scripted iLink service, with the real Gateway,
//! Workflow, and WebServer running.

#![allow(clippy::expect_used, clippy::panic)]

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use barracuda_captive_portal_plugin::{EntryState, EntryStatus, WebText};
use barracuda_imessage_gateway_plugin::ReceiveTiming;
use barracuda_platform_test::ScriptStep;
use embassy_time::Duration;
use rusty_fork::rusty_fork_test;
use serde_json::{json, Value};

use super::LoginTiming;
use crate::bench::{run, scenario, Setup, Stored, UNREACHABLE_API_BASE};
use crate::receive::PollTiming;
use crate::{decode_configuration, Timing, CONFIGURATION_STORAGE_KEY};

const QR_URL_1: &str = "https://liteapp.weixin.qq.com/q/one?qrcode=qr-1&bot_type=3";
const QR_URL_2: &str = "https://liteapp.weixin.qq.com/q/two?qrcode=qr-2&bot_type=3";
const BOT_TOKEN: &str = "bot-secret-token";
const TEST_TIMING: LoginTiming = LoginTiming {
    session: Duration::from_secs(60),
    min_poll_interval: Duration::from_ticks(0),
};

/// A login service over iLink `steps`. Other receivers hold every receive
/// slot, so a linked channel reports `no_slot` and never polls.
fn setup(steps: Vec<ScriptStep>, login: LoginTiming) -> Setup {
    let mut setup = Setup::new(
        Stored::default(),
        steps,
        Timing {
            poll: PollTiming::DEVICE,
            receive: ReceiveTiming::DEVICE,
            login,
        },
    );
    setup.hold_slots = true;
    setup
}

/// The same with another `wechat` channel registered with the Gateway.
fn occupied(steps: Vec<ScriptStep>) -> Setup {
    let mut setup = setup(steps, TEST_TIMING);
    setup.occupy_channel = true;
    setup
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

/// Portal status of a linked channel while every receive slot is held.
fn linked_without_slot() -> EntryStatus {
    EntryStatus::new(
        EntryState::Attention,
        WebText {
            zh: "名额已满",
            en: "No slot",
        },
    )
}

/// `GET` on the login route, which always answers 200.
macro_rules! login_status {
    ($harness:ident) => {{
        let (code, body) = $harness.login("GET", b"").await;
        assert_eq!(code, 200);
        body
    }};
}

/// Lets the runtime run until GET on the login route reports `$expected`.
macro_rules! wait_for_status {
    ($harness:ident, $expected:expr) => {{
        let expected: &str = $expected;
        let deadline = embassy_time::Instant::now() + Duration::from_secs(5);
        loop {
            let status: Value = login_status!($harness);
            if status["status"] == expected {
                break status;
            }
            assert!(
                embassy_time::Instant::now() < deadline,
                "status stayed {status} instead of {expected}"
            );
            embassy_time::Timer::after(Duration::from_millis(1)).await;
        }
    }};
}

/// Lets the runtime run until iLink has seen `$count` requests; returns
/// their paths.
macro_rules! wait_for_requests {
    ($harness:ident, $count:expr) => {{
        let count: usize = $count;
        $harness
            .until("the iLink requests", |harness| {
                harness.request_paths().len() >= count
            })
            .await;
        $harness.request_paths()
    }};
}

rusty_fork_test! {
#[test]
fn get_before_any_session_is_idle() {
    scenario!(Body, |harness| {
        assert_eq!(
            login_status!(harness),
            json!({ "status": "idle", "configured": false })
        );
        assert!(harness.request_paths().is_empty());
    });
    run(setup(Vec::new(), TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn start_returns_the_qr_url_and_reports_wait() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), long_poll()]);
    scenario!(Body, |harness| {
        let (code, body) = harness.login("POST", b"").await;

        assert_eq!(code, 200);
        assert_eq!(body, json!({ "url": QR_URL_1, "expires_in": 480 }));
        assert_eq!(
            wait_for_status!(harness, "wait"),
            json!({ "status": "wait", "configured": false })
        );
        assert_eq!(
            wait_for_requests!(harness, 2),
            [
                "/ilink/bot/get_bot_qrcode?bot_type=3",
                "/ilink/bot/get_qrcode_status?qrcode=qr-1",
            ]
        );
    });
    run(setup(steps, TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn scaned_is_reported_as_scanned() {
    let steps = Vec::from([
        qrcode("qr-1", QR_URL_1),
        status("wait"),
        status("scaned"),
        long_poll(),
    ]);
    scenario!(Body, |harness| {
        let (code, _body) = harness.login("POST", b"{}").await;

        assert_eq!(code, 200);
        assert_eq!(
            wait_for_status!(harness, "scanned"),
            json!({ "status": "scanned", "configured": false })
        );
    });
    run(setup(steps, TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn confirmed_login_stores_and_registers_the_channel() {
    let steps = Vec::from([
        qrcode("qr-1", QR_URL_1),
        status("wait"),
        status("scaned"),
        confirmed(),
    ]);
    scenario!(Body, |harness| {
        assert!(!harness.registered());
        let (code, body) = harness.login("POST", b"").await;
        assert_eq!(code, 200);
        assert!(!body.to_string().contains(BOT_TOKEN));

        let status = wait_for_status!(harness, "confirmed");

        assert_eq!(status, json!({ "status": "confirmed", "configured": true }));
        assert!(!status.to_string().contains(BOT_TOKEN));
        assert!(harness.registered());
        let stored = harness
            .stored(CONFIGURATION_STORAGE_KEY)
            .await
            .expect("configuration stored");
        let config =
            decode_configuration(stored.as_bytes()).expect("stored configuration decodes");
        assert_eq!(config.token, BOT_TOKEN);
        assert_eq!(config.api_base, "http://ilink.example");
        assert_eq!(config.app_id, "bot");
        assert_eq!(config.client_version, "131329");
        assert_eq!(config.route_tag, None);
        assert_eq!(config.x_wechat_uin, "MA==");
        harness.settle().await;
        assert_eq!(harness.request_paths().len(), 4, "the session ended");
    });
    run(setup(steps, TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn expired_qr_code_ends_the_session() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), status("expired")]);
    scenario!(Body, |harness| {
        let (code, _body) = harness.login("POST", b"").await;
        assert_eq!(code, 200);

        assert_eq!(
            wait_for_status!(harness, "expired"),
            json!({ "status": "expired", "configured": false })
        );
        harness.settle().await;
        assert_eq!(harness.request_paths().len(), 2, "the session ended");
        assert!(harness.stored(CONFIGURATION_STORAGE_KEY).await.is_none());
    });
    run(setup(steps, TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn session_deadline_reports_expired() {
    let timing = LoginTiming {
        session: Duration::from_millis(50),
        ..TEST_TIMING
    };
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), long_poll()]);
    scenario!(Body, |harness| {
        let (code, _body) = harness.login("POST", b"").await;
        assert_eq!(code, 200);

        assert_eq!(wait_for_status!(harness, "expired")["status"], "expired");
    });
    run(setup(steps, timing), Body);
}
}

rusty_fork_test! {
#[test]
fn delete_cancels_the_session() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), long_poll()]);
    scenario!(Body, |harness| {
        let (code, _body) = harness.login("POST", b"").await;
        assert_eq!(code, 200);
        wait_for_requests!(harness, 2);

        let (code, body) = harness.login("DELETE", b"").await;

        assert_eq!(code, 204);
        assert_eq!(body, Value::Null);
        assert_eq!(
            login_status!(harness),
            json!({ "status": "idle", "configured": false })
        );
        harness.settle().await;
        assert_eq!(login_status!(harness)["status"], "idle");
        assert_eq!(harness.request_paths().len(), 2);
    });
    run(setup(steps, TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn new_start_replaces_the_running_session() {
    let steps = Vec::from([
        qrcode("qr-1", QR_URL_1),
        long_poll(),
        qrcode("qr-2", QR_URL_2),
        long_poll(),
    ]);
    scenario!(Body, |harness| {
        let (_code, first) = harness.login("POST", b"").await;
        assert_eq!(first["url"], QR_URL_1);
        wait_for_requests!(harness, 2);

        let (code, second) = harness.login("POST", b"").await;

        assert_eq!(code, 200);
        assert_eq!(second["url"], QR_URL_2);
        let requests = wait_for_requests!(harness, 4);
        assert_eq!(
            requests.get(3).map(String::as_str),
            Some("/ilink/bot/get_qrcode_status?qrcode=qr-2")
        );
        assert_eq!(login_status!(harness)["status"], "wait");
    });
    run(setup(steps, TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn unreachable_upstream_returns_502() {
    let mut setup = setup(Vec::new(), TEST_TIMING);
    setup.login_api_base = UNREACHABLE_API_BASE;
    scenario!(Body, |harness| {
        let (code, body) = harness.login("POST", b"").await;

        assert_eq!(code, 502);
        assert_eq!(body["error"], "upstream_unavailable");
        assert!(body["message"].is_string());
        assert_eq!(login_status!(harness)["status"], "idle");
    });
    run(setup, Body);
}
}

rusty_fork_test! {
#[test]
fn upstream_error_code_and_message_pass_through() {
    let steps = Vec::from([ScriptStep::json(
        200,
        r#"{"ret":-14,"errmsg":"session timeout"}"#,
    )]);
    scenario!(Body, |harness| {
        let (code, body) = harness.login("POST", b"").await;

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
    run(setup(steps, TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn failed_status_poll_reports_failed() {
    let steps = Vec::from([
        qrcode("qr-1", QR_URL_1),
        ScriptStep::json(200, r#"{"ret":1,"errmsg":"bad qrcode"}"#),
    ]);
    scenario!(Body, |harness| {
        harness.login("POST", b"").await;

        let status = wait_for_status!(harness, "failed");

        assert_eq!(status["configured"], false);
        assert!(status["message"]
            .as_str()
            .is_some_and(|message| message.contains("bad qrcode")));
    });
    run(setup(steps, TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn rejected_registration_reports_failed() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), confirmed()]);
    scenario!(Body, |harness| {
        harness.login("POST", b"").await;

        let status = wait_for_status!(harness, "failed");

        assert_eq!(status["configured"], false);
        assert!(status["message"].is_string());
        assert!(!status.to_string().contains(BOT_TOKEN));
        assert!(harness.stored(CONFIGURATION_STORAGE_KEY).await.is_none());
    });
    run(occupied(steps), Body);
}
}

rusty_fork_test! {
#[test]
fn configured_reflects_the_registered_channel() {
    scenario!(Body, |harness| {
        let (code, _body) = harness.config("POST", br#"{"token":"manual-token"}"#).await;
        assert_eq!(code, 204);

        assert_eq!(
            login_status!(harness),
            json!({ "status": "idle", "configured": true })
        );
    });
    run(setup(Vec::new(), TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn wrong_methods_return_405() {
    scenario!(Body, |harness| {
        for method in ["PUT", "PATCH", "OPTIONS"] {
            let (code, body) = harness.login(method, b"").await;
            assert_eq!(code, 405);
            assert_eq!(body, json!({ "error": "method_not_allowed" }));
        }
        for method in ["PUT", "DELETE"] {
            let (code, body) = harness.config(method, b"").await;
            assert_eq!(code, 405);
            assert_eq!(body, json!({ "error": "method_not_allowed" }));
        }
    });
    run(setup(Vec::new(), TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn status_reports_only_whether_a_channel_is_configured() {
    scenario!(Body, |harness| {
        // The configuration path takes changes only; its state is under `/status`.
        let (code, _body) = harness.config("GET", b"").await;
        assert_eq!(code, 405);
        let (code, body) = harness.status().await;
        assert_eq!(code, 200);
        assert_eq!(body["configured"], false);
        assert_eq!(harness.entry_status(), EntryStatus::configured(false));

        let (code, _body) = harness.config("POST", br#"{"token":"manual-token"}"#).await;
        assert_eq!(code, 204);

        let (code, body) = harness.status().await;
        assert_eq!(code, 200);
        assert_eq!(body["configured"], true);
        assert_eq!(body["mode"], "send_receive");
        assert!(!body.to_string().contains("manual-token"));
        assert_eq!(harness.entry_status(), linked_without_slot());
    });
    run(setup(Vec::new(), TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn config_rejected_by_the_gateway_is_registration_failed() {
    scenario!(Body, |harness| {
        let (code, body) = harness.config("POST", br#"{"token":"manual-token"}"#).await;

        assert_eq!(code, 422);
        assert_eq!(body, json!({ "error": "registration_failed" }));
        assert!(harness.stored(CONFIGURATION_STORAGE_KEY).await.is_none());
        let (_code, body) = harness.status().await;
        assert_eq!(body["configured"], false);
        assert_eq!(harness.entry_status(), EntryStatus::configured(false));
    });
    run(occupied(Vec::new()), Body);
}
}

rusty_fork_test! {
#[test]
fn entry_status_waits_for_the_scan_until_the_login_is_confirmed() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), status("scaned"), long_poll()]);
    scenario!(Body, |harness| {
        let waiting = EntryStatus::new(
            EntryState::Attention,
            WebText {
                zh: "等待扫码",
                en: "Waiting for scan",
            },
        );
        assert_eq!(harness.entry_status(), EntryStatus::configured(false));
        let (code, _body) = harness.login("POST", b"").await;
        assert_eq!(code, 200);
        // The session reports `wait` before the start request is answered.
        assert_eq!(harness.entry_status(), waiting);
        wait_for_status!(harness, "scanned");
        assert_eq!(harness.entry_status(), waiting);

        harness.login("DELETE", b"").await;
        assert_eq!(harness.entry_status(), EntryStatus::configured(false));
    });
    run(setup(steps, TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn entry_status_is_ready_after_a_confirmed_login() {
    let steps = Vec::from([qrcode("qr-1", QR_URL_1), confirmed()]);
    scenario!(Body, |harness| {
        harness.login("POST", b"").await;
        wait_for_status!(harness, "confirmed");
        assert_eq!(harness.entry_status(), linked_without_slot());
    });
    run(setup(steps, TEST_TIMING), Body);
}
}

rusty_fork_test! {
#[test]
fn start_rejects_a_body_with_fields() {
    scenario!(Body, |harness| {
        let (code, body) = harness.login("POST", br#"{"token":"x"}"#).await;

        assert_eq!(code, 400);
        assert_eq!(body, json!({ "error": "invalid_request" }));
        assert!(harness.request_paths().is_empty());
    });
    run(setup(Vec::new(), TEST_TIMING), Body);
}
}

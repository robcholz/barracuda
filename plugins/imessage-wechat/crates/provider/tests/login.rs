#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! The iLink QR login calls against a scripted iLink service, through a real
//! Gateway's HTTP on a loopback network.

mod support;

use barracuda_imessage_gateway_plugin::GatewayHttp;
use barracuda_platform_test::{ScriptStep, ScriptedServer};
use support::{bench, API_BASE};
use wechat::{LoginError, LoginStatus, WechatLogin};

/// A port on the loopback network nobody listens on.
const UNREACHABLE_API_BASE: &str = "http://10.0.0.1:9001";

/// A login client for the scripted service; the trailing slash is trimmed.
fn login(http: GatewayHttp) -> WechatLogin {
    WechatLogin::new(http, format!("{API_BASE}/"))
}

fn request_line(network: &ScriptedServer, index: usize) -> String {
    network.requests()[index]
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn qrcode_returns_the_id_and_scan_url() {
    let steps = vec![ScriptStep::json(
        200,
        r#"{"qrcode":"qr-1","qrcode_img_content":"https://liteapp.weixin.qq.com/q/x?qrcode=qr-1&bot_type=3","ret":0}"#,
    )];
    bench(steps, async |http, network| {
        let qrcode = login(http).qrcode().await.expect("QR code");

        assert_eq!(qrcode.id, "qr-1");
        assert_eq!(
            qrcode.url,
            "https://liteapp.weixin.qq.com/q/x?qrcode=qr-1&bot_type=3"
        );
        assert_eq!(
            request_line(network, 0),
            "GET /ilink/bot/get_bot_qrcode?bot_type=3 HTTP/1.1"
        );
        // iLink answers a chunked GET with HTTP 412.
        let request = network.requests()[0].to_ascii_lowercase();
        assert!(!request.contains("transfer-encoding"), "{request}");
    });
}

#[test]
fn status_maps_every_ilink_state() {
    let steps = vec![
        ScriptStep::json(200, r#"{"ret":0,"status":"wait"}"#),
        ScriptStep::json(200, r#"{"ret":0,"status":"scaned"}"#),
        ScriptStep::json(200, r#"{"ret":0,"status":"expired"}"#),
        ScriptStep::json(
            200,
            r#"{"ret":0,"status":"confirmed","bot_token":"secret","ilink_bot_id":"bot@im.bot","baseurl":"https://ilink.example"}"#,
        ),
    ];
    bench(steps, async |http, network| {
        let login = login(http);

        assert!(matches!(login.status("a b+c").await, Ok(LoginStatus::Wait)));
        assert!(matches!(login.status("qr").await, Ok(LoginStatus::Scanned)));
        assert!(matches!(login.status("qr").await, Ok(LoginStatus::Expired)));
        let Ok(LoginStatus::Confirmed(credentials)) = login.status("qr").await else {
            panic!("confirmed login");
        };

        assert_eq!(credentials.bot_token, "secret");
        assert_eq!(credentials.bot_id.as_deref(), Some("bot@im.bot"));
        assert_eq!(
            credentials.base_url.as_deref(),
            Some("https://ilink.example")
        );
        assert_eq!(
            request_line(network, 0),
            "GET /ilink/bot/get_qrcode_status?qrcode=a%20b%2Bc HTTP/1.1"
        );
    });
}

#[test]
fn ilink_errors_keep_the_upstream_code_and_message() {
    let steps = vec![ScriptStep::json(
        200,
        r#"{"ret":-14,"errmsg":"session timeout"}"#,
    )];
    bench(steps, async |http, _network| {
        let error = login(http).qrcode().await.err().expect("iLink error");

        assert!(matches!(error, LoginError::Platform { .. }));
        assert_eq!(error.code(), Some("-14"));
        assert_eq!(error.message(), "session timeout");
    });
}

#[test]
fn transport_and_non_json_failures_are_transport_errors() {
    let steps = vec![
        ScriptStep::response(200, "text/html", b"<html></html>", usize::MAX),
        ScriptStep::json(503, "{}"),
        ScriptStep::json(200, r#"{"ret":0,"status":"later"}"#),
    ];
    bench(steps, async |http, _network| {
        let reachable = login(http.clone());
        for _ in 0..3 {
            let error = reachable.status("qr").await.err().expect("failure");
            assert!(matches!(error, LoginError::Transport { .. }), "{error}");
            assert_eq!(error.code(), None);
        }
        let unreachable = WechatLogin::new(http, UNREACHABLE_API_BASE);
        let error = unreachable.qrcode().await.err().expect("no connection");
        assert!(matches!(error, LoginError::Transport { .. }));
    });
}

#[test]
fn confirmed_without_a_token_is_rejected() {
    let steps = vec![ScriptStep::json(200, r#"{"ret":0,"status":"confirmed"}"#)];
    bench(steps, async |http, _network| {
        assert!(matches!(
            login(http).status("qr").await,
            Err(LoginError::Transport { .. })
        ));
    });
}

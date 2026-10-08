#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use barracuda_platform_test::{ScriptStep, ScriptedStack};
use futures_lite::future::block_on;
use http_client::ClientFactory;
use wechat::{LoginError, LoginStatus, WechatLogin};

fn login(
    steps: Vec<ScriptStep>,
) -> (
    &'static ScriptedStack,
    WechatLogin<'static, ScriptedStack, ScriptedStack>,
) {
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new(steps)));
    let login = WechatLogin::new(
        ClientFactory::from_network(network, network),
        "http://wechat.test/",
    );
    (network, login)
}

fn request_line(network: &ScriptedStack, index: usize) -> String {
    network.requests()[index]
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn qrcode_returns_the_id_and_scan_url() {
    let (network, login) = login(vec![ScriptStep::json(
        200,
        r#"{"qrcode":"qr-1","qrcode_img_content":"https://liteapp.weixin.qq.com/q/x?qrcode=qr-1&bot_type=3","ret":0}"#,
    )]);

    let qrcode = block_on(login.qrcode()).expect("QR code");

    assert_eq!(qrcode.id, "qr-1");
    assert_eq!(
        qrcode.url,
        "https://liteapp.weixin.qq.com/q/x?qrcode=qr-1&bot_type=3"
    );
    assert_eq!(
        request_line(network, 0),
        "GET /ilink/bot/get_bot_qrcode?bot_type=3 HTTP/1.1"
    );
}

#[test]
fn status_maps_every_ilink_state() {
    let (network, login) = login(vec![
        ScriptStep::json(200, r#"{"ret":0,"status":"wait"}"#),
        ScriptStep::json(200, r#"{"ret":0,"status":"scaned"}"#),
        ScriptStep::json(200, r#"{"ret":0,"status":"expired"}"#),
        ScriptStep::json(
            200,
            r#"{"ret":0,"status":"confirmed","bot_token":"secret","ilink_bot_id":"bot@im.bot","baseurl":"https://ilink.example"}"#,
        ),
    ]);

    assert!(matches!(
        block_on(login.status("a b+c")),
        Ok(LoginStatus::Wait)
    ));
    assert!(matches!(
        block_on(login.status("qr")),
        Ok(LoginStatus::Scanned)
    ));
    assert!(matches!(
        block_on(login.status("qr")),
        Ok(LoginStatus::Expired)
    ));
    let Ok(LoginStatus::Confirmed(credentials)) = block_on(login.status("qr")) else {
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
}

#[test]
fn ilink_errors_keep_the_upstream_code_and_message() {
    let (_network, login) = login(vec![ScriptStep::json(
        200,
        r#"{"ret":-14,"errmsg":"session timeout"}"#,
    )]);

    let error = block_on(login.qrcode()).err().expect("iLink error");

    assert!(matches!(error, LoginError::Platform { .. }));
    assert_eq!(error.code(), Some("-14"));
    assert_eq!(error.message(), "session timeout");
}

#[test]
fn transport_and_non_json_failures_are_transport_errors() {
    let (_network, login) = login(vec![
        ScriptStep::response(200, "text/html", b"<html></html>", usize::MAX),
        ScriptStep::json(503, "{}"),
        ScriptStep::json(200, r#"{"ret":0,"status":"later"}"#),
    ]);

    for _ in 0..3 {
        let error = block_on(login.status("qr")).err().expect("failure");
        assert!(matches!(error, LoginError::Transport { .. }), "{error}");
        assert_eq!(error.code(), None);
    }
    let error = block_on(login.qrcode()).err().expect("no connection");
    assert!(matches!(error, LoginError::Transport { .. }));
}

#[test]
fn confirmed_without_a_token_is_rejected() {
    let (_network, login) = login(vec![ScriptStep::json(
        200,
        r#"{"ret":0,"status":"confirmed"}"#,
    )]);

    assert!(matches!(
        block_on(login.status("qr")),
        Err(LoginError::Transport { .. })
    ));
}

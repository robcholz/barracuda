#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use barracuda_platform_test::{ScriptStep, ScriptedStack};
use futures_lite::future::block_on;
use http_client::ClientFactory;
use qq::bind::{connect_url, BindError, BindKey, BindStatus, QQBind};
use std::boxed::Box;

/// `qq-app-secret` sealed with [`key`] under nonce `07…07`.
const SEALED: &str = "BwcHBwcHBwcHBwcHfhuLPR9/+oej/mHl9NkpjVTRnZ48x3ZdcxN3+Kw=";

fn key() -> BindKey {
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::try_from(index).expect("index");
    }
    BindKey::new(bytes)
}

fn bind(
    steps: impl IntoIterator<Item = ScriptStep>,
) -> (
    &'static ScriptedStack,
    QQBind<'static, ScriptedStack, ScriptedStack>,
) {
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new(steps)));
    (
        network,
        QQBind::new(
            ClientFactory::from_network(network, network),
            "http://portal.test",
        ),
    )
}

fn poll(body: &str) -> Result<BindStatus, BindError> {
    let (_network, bind) = bind([ScriptStep::json(200, body)]);
    block_on(bind.poll("task-1", &key()))
}

#[test]
fn create_sends_the_key_and_returns_the_task() {
    let (network, bind) = bind([ScriptStep::json(
        200,
        r#"{"retcode":0,"msg":"success","data":{"task_id":"ef0f-27"}}"#,
    )]);
    assert_eq!(block_on(bind.create(&key())).expect("task"), "ef0f-27");
    let request = &network.requests()[0];
    assert!(
        request.starts_with("POST /lite/create_bind_task HTTP/1.1"),
        "{request}"
    );
    assert!(request.contains("Accept: application/json"), "{request}");
    assert!(
        request.ends_with(r#"{"key":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="}"#),
        "{request}"
    );
}

#[test]
fn a_task_nobody_completed_waits_and_an_ended_one_expires() {
    let waiting = r#"{"retcode":0,"msg":"success","data":{"status":1,"bot_appid":"0","bot_encrypt_secret":"","user_openid":""}}"#;
    assert!(matches!(poll(waiting), Ok(BindStatus::Waiting)));
    assert!(matches!(
        poll(r#"{"retcode":0,"data":{"status":0}}"#),
        Ok(BindStatus::Waiting)
    ));
    assert!(matches!(
        poll(r#"{"retcode":0,"data":{"status":3,"bot_appid":"0"}}"#),
        Ok(BindStatus::Expired)
    ));
    assert!(matches!(
        poll(r#"{"retcode":0,"data":{"status":9}}"#),
        Err(BindError::Transport { .. })
    ));
}

#[test]
fn a_completed_binding_opens_the_secret_with_the_task_key() {
    let (network, bind) = bind([ScriptStep::json(
        200,
        &format!(
            r#"{{"retcode":0,"data":{{"status":2,"bot_appid":102345678,"bot_encrypt_secret":"{SEALED}","user_openid":"7F3A9B2E"}}}}"#
        ),
    )]);
    let Ok(BindStatus::Completed(bot)) = block_on(bind.poll("task 1", &key())) else {
        panic!("completed binding");
    };
    assert_eq!(bot.app_id, "102345678");
    assert_eq!(bot.app_secret, "qq-app-secret");
    assert_eq!(bot.user_openid.as_deref(), Some("7F3A9B2E"));
    let request = &network.requests()[0];
    assert!(
        request.starts_with("POST /lite/poll_bind_result HTTP/1.1"),
        "{request}"
    );
    assert!(request.ends_with(r#"{"task_id":"task 1"}"#), "{request}");
}

#[test]
fn a_secret_sealed_to_another_key_does_not_open() {
    let mut other = [9_u8; 32];
    other[0] = 1;
    let (_network, bind) = bind([ScriptStep::json(
        200,
        &format!(
            r#"{{"retcode":0,"data":{{"status":2,"bot_appid":"102345678","bot_encrypt_secret":"{SEALED}"}}}}"#
        ),
    )]);
    assert!(matches!(
        block_on(bind.poll("task-1", &BindKey::new(other))),
        Err(BindError::Secret)
    ));
    assert!(matches!(
        poll(r#"{"retcode":0,"data":{"status":2,"bot_appid":"1","bot_encrypt_secret":"AAAA"}}"#),
        Err(BindError::Secret)
    ));
    assert!(matches!(
        poll(r#"{"retcode":0,"data":{"status":2,"bot_appid":"0","bot_encrypt_secret":"AAAA"}}"#),
        Err(BindError::Transport { .. })
    ));
}

#[test]
fn portal_errors_pass_their_code_and_message_through() {
    let Err(error) = poll(r#"{"retcode":10001,"msg":"task not found"}"#) else {
        panic!("portal error");
    };
    assert_eq!(error.code(), Some("10001"));
    assert_eq!(error.message(), "task not found");
    assert!(matches!(
        {
            let (_network, bind) = bind([ScriptStep::json(502, "{}")]);
            block_on(bind.create(&key()))
        },
        Err(BindError::Transport { .. })
    ));
}

#[test]
fn connect_url_names_the_task_and_this_device() {
    assert_eq!(
        connect_url("ef0f 27"),
        "https://q.qq.com/qqbot/openclaw/connect.html?task_id=ef0f%2027&source=barracuda&_wv=2"
    );
}

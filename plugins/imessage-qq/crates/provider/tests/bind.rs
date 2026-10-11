#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! The scan-to-bind calls against a scripted portal, through a real
//! Gateway's HTTP on a loopback network.

mod support;

use barracuda_platform_test::ScriptStep;
use qq::bind::{connect_url, BindError, BindKey, BindStatus, QQBind};
use support::{bench, ORIGIN};

/// `qq-app-secret` sealed with [`key`] under nonce `07…07`.
const SEALED: &str = "BwcHBwcHBwcHBwcHfhuLPR9/+oej/mHl9NkpjVTRnZ48x3ZdcxN3+Kw=";

fn key() -> BindKey {
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::try_from(index).expect("index");
    }
    BindKey::new(bytes)
}

/// Polls task `task-1` against a portal answering `body`.
fn poll(body: &str) -> Result<BindStatus, BindError> {
    let mut status = None;
    bench([ScriptStep::json(200, body)], async |http, _server| {
        let bind = QQBind::new(http, ORIGIN);
        status = Some(bind.poll("task-1", &key()).await);
    });
    status.expect("polled")
}

#[test]
fn create_sends_the_key_and_returns_the_task() {
    bench(
        [ScriptStep::json(
            200,
            r#"{"retcode":0,"msg":"success","data":{"task_id":"ef0f-27"}}"#,
        )],
        async |http, server| {
            let bind = QQBind::new(http, ORIGIN);
            assert_eq!(bind.create(&key()).await.expect("task"), "ef0f-27");
            let request = &server.requests()[0];
            assert!(
                request.starts_with("POST /lite/create_bind_task HTTP/1.1"),
                "{request}"
            );
            assert!(request.contains("Accept: application/json"), "{request}");
            assert!(
                request.ends_with(r#"{"key":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="}"#),
                "{request}"
            );
        },
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
    bench(
        [ScriptStep::json(
            200,
            &format!(
                r#"{{"retcode":0,"data":{{"status":2,"bot_appid":102345678,"bot_encrypt_secret":"{SEALED}","user_openid":"7F3A9B2E"}}}}"#
            ),
        )],
        async |http, server| {
            let bind = QQBind::new(http, ORIGIN);
            let Ok(BindStatus::Completed(bot)) = bind.poll("task 1", &key()).await else {
                panic!("completed binding");
            };
            assert_eq!(bot.app_id, "102345678");
            assert_eq!(bot.app_secret, "qq-app-secret");
            assert_eq!(bot.user_openid.as_deref(), Some("7F3A9B2E"));
            let request = &server.requests()[0];
            assert!(
                request.starts_with("POST /lite/poll_bind_result HTTP/1.1"),
                "{request}"
            );
            assert!(request.ends_with(r#"{"task_id":"task 1"}"#), "{request}");
        },
    );
}

#[test]
fn a_secret_sealed_to_another_key_does_not_open() {
    let mut other = [9_u8; 32];
    other[0] = 1;
    bench(
        [ScriptStep::json(
            200,
            &format!(
                r#"{{"retcode":0,"data":{{"status":2,"bot_appid":"102345678","bot_encrypt_secret":"{SEALED}"}}}}"#
            ),
        )],
        async |http, _server| {
            let bind = QQBind::new(http, ORIGIN);
            assert!(matches!(
                bind.poll("task-1", &BindKey::new(other)).await,
                Err(BindError::Secret)
            ));
        },
    );
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
    bench([ScriptStep::json(502, "{}")], async |http, _server| {
        let bind = QQBind::new(http, ORIGIN);
        assert!(matches!(
            bind.create(&key()).await,
            Err(BindError::Transport { .. })
        ));
    });
}

#[test]
fn connect_url_names_the_task_and_this_device() {
    assert_eq!(
        connect_url("ef0f 27"),
        "https://q.qq.com/qqbot/openclaw/connect.html?task_id=ef0f%2027&source=barracuda&_wv=2"
    );
}

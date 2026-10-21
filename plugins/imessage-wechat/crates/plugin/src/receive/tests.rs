//! The receive loop against a scripted iLink service, with the real
//! Gateway, Workflow, and WebServer running.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use barracuda_captive_portal_plugin::{EntryState, EntryStatus, WebText};
use barracuda_imessage_gateway_plugin::{
    ChannelControl, ChannelMode, MessageChannel, MessageTarget, ReceiveFailure, ReceiveState,
    ReceiveTiming, SendMessageRequest, PAIRED_REPLY,
};
use barracuda_platform_test::ScriptStep;
use embassy_time::Duration;
use embedded_io_async::ErrorKind;
use rusty_fork::rusty_fork_test;
use serde_json::{json, Value};

use super::{
    PollTiming, BOT_ID_STORAGE_KEY, CONTEXT_TOKENS_STORAGE_KEY, CURSOR_STORAGE_KEY,
    SESSION_EXPIRED_CODE, SESSION_EXPIRED_MESSAGE,
};
use crate::bench::{run, scenario, Setup, Stored, API_BASE, PAIRING_CODE};
use crate::login::LoginTiming;
use crate::{ConfigRequest, Timing};

const OWNER: &str = "owner@im.wechat";
const STRANGER: &str = "stranger@im.wechat";

const TIMING: Timing = Timing {
    poll: PollTiming {
        margin: Duration::from_millis(20),
        min_long_poll: Duration::from_millis(1),
        max_long_poll: Duration::from_secs(120),
        idle_cursor_write: Duration::from_secs(3600),
    },
    receive: ReceiveTiming {
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(40),
        // Long enough that only a freed slot can wake the runtime in time.
        slot_retry: Duration::from_secs(3600),
    },
    login: LoginTiming::DEVICE,
};

/// A setup for the receive loop.
fn setup(stored: Stored, steps: Vec<ScriptStep>) -> Setup {
    Setup::new(stored, steps, TIMING)
}

fn text(message_id: u64, from: &str, text: &str, context_token: &str) -> Value {
    json!({
        "seq": message_id,
        "message_id": message_id,
        "from_user_id": from,
        "to_user_id": "bot@im.bot",
        "create_time_ms": 1_700_000_000_000_u64,
        "message_type": 1,
        "message_state": 2,
        "context_token": context_token,
        "item_list": [{ "type": 1, "text_item": { "text": text } }],
    })
}

fn updates(cursor: &str, msgs: Vec<Value>) -> ScriptStep {
    ScriptStep::json(
        200,
        &json!({
            "ret": 0,
            "msgs": msgs,
            "get_updates_buf": cursor,
            "longpolling_timeout_ms": 35000,
        })
        .to_string(),
    )
}

/// A long poll the server holds until the test ends.
fn hold() -> ScriptStep {
    ScriptStep::pending_after_headers(200, "application/json")
}

fn sent() -> ScriptStep {
    ScriptStep::json(200, r#"{"ret":0}"#)
}

rusty_fork_test! {
#[test]
fn owner_text_is_published_on_the_sender_route() {
    let steps = Vec::from([
        updates(
            "cursor-1",
            Vec::from([
                text(18_446_744_073_709_551_615, OWNER, "hello", "ctx-1"),
                json!({ "message_id": 2, "from_user_id": "bot@im.bot", "message_type": 2,
                        "item_list": [{ "type": 1, "text_item": { "text": "echo" } }] }),
                json!({ "message_id": 3, "from_user_id": OWNER, "message_type": 1,
                        "item_list": [{ "type": 11, "tool_call_start_item": {} }] }),
            ]),
        ),
        hold(),
    ]);
    let mut setup = setup(Stored::linked().with_owner(OWNER), steps);
    setup.sends = Vec::from([sent()]);
    scenario!(Body, |harness| {
        harness
            .until("the second poll and the published message", |harness| {
                harness.requests().len() == 2 && !harness.published().is_empty()
            })
            .await;
        harness.settle().await;

        assert_eq!(
            harness.published(),
            [(OWNER.into(), "18446744073709551615".into(), "hello".into())]
        );
        assert_eq!(harness.state(), ReceiveState::Receiving);
        assert_eq!(harness.channel.inbound.skipped.get(), 2);
        let requests = harness.requests();
        let first = &requests[0];
        assert_eq!(first.path, "/ilink/bot/getupdates");
        assert_eq!(first.header("Authorization"), Some("Bearer bot-token"));
        assert_eq!(first.header("AuthorizationType"), Some("ilink_bot_token"));
        assert_eq!(first.header("iLink-App-Id"), Some("bot"));
        assert_eq!(first.header("iLink-App-ClientVersion"), Some("131329"));
        let uins: Vec<&str> = requests
            .iter()
            .map(|request| request.header("X-WECHAT-UIN").expect("UIN header"))
            .collect();
        assert_ne!(uins[0], uins[1], "a new UIN per request");
        assert_eq!(first.json()["get_updates_buf"], "");
        assert_eq!(requests[1].json()["get_updates_buf"], "cursor-1");
        assert_eq!(
            harness.server.connect_count(),
            1,
            "one kept-alive connection"
        );
        assert_eq!(
            harness.stored(CURSOR_STORAGE_KEY).await.as_deref(),
            Some("cursor-1")
        );
        let tokens: Value = serde_json::from_str(
            &harness
                .stored(CONTEXT_TOKENS_STORAGE_KEY)
                .await
                .expect("tokens stored"),
        )
        .expect("token JSON");
        assert_eq!(tokens, json!([{ "user": OWNER, "token": "ctx-1" }]));

        // A reply on the route carries the stored token without a thread.
        let provider = harness.channel.provider().expect("linked");
        provider
            .send_message(SendMessageRequest::text(
                MessageTarget::new("wechat", OWNER),
                "hi back",
            ))
            .await
            .expect("send");
        let reply = harness.send_requests().pop().expect("send request").json();
        assert_eq!(reply["msg"]["to_user_id"], OWNER);
        assert_eq!(reply["msg"]["context_token"], "ctx-1");
    });
    run(setup, Body);
}
}

rusty_fork_test! {
#[test]
fn pairing_code_adds_the_sender_and_replies_once() {
    let steps = Vec::from([
        updates(
            "cursor-1",
            Vec::from([text(7, OWNER, PAIRING_CODE, "ctx-7")]),
        ),
        hold(),
    ]);
    let mut setup = setup(Stored::linked(), steps);
    setup.sends = Vec::from([sent()]);
    scenario!(Body, |harness| {
        harness
            .until("the pairing reply", |harness| {
                !harness.send_requests().is_empty()
            })
            .await;
        harness.settle().await;

        assert!(harness.channel.owners().expect("owners").is_owner(OWNER));
        assert!(harness.published().is_empty(), "the code is not published");
        let replies = harness.send_requests();
        assert_eq!(replies.len(), 1, "one reply");
        let reply = replies[0].json();
        assert_eq!(reply["msg"]["to_user_id"], OWNER);
        assert_eq!(reply["msg"]["context_token"], "ctx-7");
        assert_eq!(
            reply["msg"]["item_list"][0]["text_item"]["text"],
            PAIRED_REPLY
        );
    });
    run(setup, Body);
}
}

rusty_fork_test! {
#[test]
fn strangers_are_dropped_without_a_reply_or_token() {
    let steps = Vec::from([
        updates("cursor-1", Vec::from([text(9, STRANGER, "hi", "ctx-9")])),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("the second poll", |harness| harness.requests().len() == 2)
            .await;
        harness.settle().await;

        assert!(harness.published().is_empty());
        assert!(harness.send_requests().is_empty());
        assert_eq!(harness.channel.owners().expect("owners").ignored(), 1);
        assert_eq!(harness.channel.inbound.context_tokens.get(STRANGER), None);
        assert_eq!(harness.stored(CONTEXT_TOKENS_STORAGE_KEY).await, None);
    });
    run(setup(Stored::linked().with_owner(OWNER), steps), Body);
}
}

rusty_fork_test! {
#[test]
fn redelivered_messages_are_dropped_after_a_reconnect() {
    let steps = Vec::from([
        updates("cursor-1", Vec::from([text(1, OWNER, "one", "ctx")])),
        // The server closes the kept-alive connection.
        ScriptStep::ConnectError(ErrorKind::ConnectionReset),
        updates(
            "cursor-2",
            Vec::from([text(1, OWNER, "one", "ctx"), text(2, OWNER, "two", "ctx")]),
        ),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("the poll after both messages", |harness| {
                harness.requests().len() == 4 && harness.published().len() >= 2
            })
            .await;
        harness.settle().await;

        assert_eq!(
            harness.published(),
            [
                (OWNER.into(), "1".into(), "one".into()),
                (OWNER.into(), "2".into(), "two".into()),
            ]
        );
        assert_eq!(harness.server.connect_count(), 2, "reconnected at once");
        assert_eq!(harness.state(), ReceiveState::Receiving);
    });
    run(setup(Stored::linked().with_owner(OWNER), steps), Body);
}
}

rusty_fork_test! {
#[test]
fn cursor_is_restored_and_idle_cursors_are_written_sparingly() {
    let steps = Vec::from([
        updates("cursor-1", Vec::from([text(1, OWNER, "one", "ctx")])),
        updates("cursor-2", Vec::new()),
        hold(),
    ]);
    let stored = Stored {
        cursor: Some("cursor-0"),
        ..Stored::linked().with_owner(OWNER)
    };
    scenario!(Body, |harness| {
        harness
            .until("the third poll", |harness| harness.requests().len() == 3)
            .await;

        let bodies = harness.poll_bodies();
        assert_eq!(bodies[0]["get_updates_buf"], "cursor-0");
        assert_eq!(bodies[1]["get_updates_buf"], "cursor-1");
        assert_eq!(bodies[2]["get_updates_buf"], "cursor-2");
        // A batch with messages is stored at once; an empty one waits.
        assert_eq!(
            harness.stored(CURSOR_STORAGE_KEY).await.as_deref(),
            Some("cursor-1")
        );
    });
    run(setup(stored, steps), Body);
}
}

rusty_fork_test! {
#[test]
fn an_oversized_batch_is_skipped_past_its_cursor_and_counted() {
    let huge = "x".repeat(wechat::MAX_UPDATES_BYTES);
    let steps = Vec::from([
        updates("cursor-big", Vec::from([text(1, OWNER, &huge, "ctx")])),
        updates("cursor-2", Vec::from([text(2, OWNER, "after", "ctx")])),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("the message after the skipped batch", |harness| {
                !harness.published().is_empty() && harness.requests().len() == 3
            })
            .await;
        harness.settle().await;

        assert_eq!(
            harness.published(),
            [(OWNER.into(), "2".into(), "after".into())]
        );
        assert_eq!(harness.channel.inbound.oversized.get(), 1);
        assert_eq!(harness.state(), ReceiveState::Receiving);
        let bodies = harness.poll_bodies();
        assert_eq!(bodies[1]["get_updates_buf"], "cursor-big");
        assert_eq!(bodies[2]["get_updates_buf"], "cursor-2");
        assert_eq!(harness.server.connect_count(), 1, "the connection is kept");
        assert_eq!(
            harness.stored(CURSOR_STORAGE_KEY).await.as_deref(),
            Some("cursor-2")
        );
    });
    run(setup(Stored::linked().with_owner(OWNER), steps), Body);
}
}

rusty_fork_test! {
#[test]
fn an_unanswered_long_poll_is_an_empty_success() {
    let short = ScriptStep::json(
        200,
        r#"{"ret":0,"msgs":[],"get_updates_buf":"cursor-1","longpolling_timeout_ms":5}"#,
    );
    let steps = Vec::from([
        short,
        hold(),
        updates("cursor-2", Vec::from([text(4, OWNER, "late", "ctx")])),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("the message after the timeout", |harness| {
                !harness.published().is_empty()
            })
            .await;

        assert_eq!(harness.server.connect_count(), 2);
        assert_eq!(harness.state(), ReceiveState::Receiving);
        assert_eq!(harness.poll_bodies()[2]["get_updates_buf"], "cursor-1");
    });
    run(setup(Stored::linked().with_owner(OWNER), steps), Body);
}
}

rusty_fork_test! {
#[test]
fn ilink_errors_retry_after_backoff() {
    let steps = Vec::from([
        ScriptStep::json(200, r#"{"ret":-1,"errmsg":"busy"}"#),
        updates("cursor-1", Vec::from([text(5, OWNER, "after", "ctx")])),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("the retried poll", |harness| !harness.published().is_empty())
            .await;

        assert_eq!(harness.state(), ReceiveState::Receiving);
        assert_eq!(harness.server.connect_count(), 2);
    });
    run(setup(Stored::linked().with_owner(OWNER), steps), Body);
}
}

rusty_fork_test! {
#[test]
fn an_expired_session_halts_until_a_new_login() {
    let steps = Vec::from([
        updates("cursor-1", Vec::new()),
        ScriptStep::json(200, r#"{"ret":-14,"errmsg":"session timeout"}"#),
        updates("cursor-9", Vec::from([text(6, OWNER, "back", "ctx")])),
        hold(),
    ]);
    let stored = Stored {
        cursor: Some("cursor-0"),
        bot_id: Some("bot-1"),
        ..Stored::linked().with_owner(OWNER)
    };
    scenario!(Body, |harness| {
        harness
            .until("the halt", |harness| {
                matches!(harness.state(), ReceiveState::Error(_))
            })
            .await;
        harness.settle().await;

        assert_eq!(
            harness.state(),
            ReceiveState::Error(ReceiveFailure {
                message: SESSION_EXPIRED_MESSAGE.into(),
                code: Some(SESSION_EXPIRED_CODE),
            })
        );
        assert_eq!(harness.requests().len(), 2, "no retry while halted");
        let (code, body) = harness.status().await;
        assert_eq!(code, 200);
        assert_eq!(
            body["receive"],
            json!({
                "state": "error",
                "message": SESSION_EXPIRED_MESSAGE,
                "code": SESSION_EXPIRED_CODE,
                "slots": { "in_use": 1, "capacity": harness.capacity },
            })
        );
        assert_eq!(
            harness.entry_status(),
            EntryStatus::new(
                EntryState::Attention,
                WebText {
                    zh: "需要重新扫码",
                    en: "Scan again to relink",
                },
            )
        );

        // A QR login confirmed for another bot clears the old bot's state.
        harness
            .channel
            .inbound
            .context_tokens
            .remember(OWNER, "old");
        let config = ConfigRequest::from_login("new-token".into(), API_BASE.into());
        assert!(harness.channel.link(config, Some("bot-2")).await.is_ok());
        harness
            .until("receiving again", |harness| !harness.published().is_empty())
            .await;

        let requests = harness.requests();
        let resumed = requests.get(2).expect("a poll after the login");
        assert_eq!(resumed.header("Authorization"), Some("Bearer new-token"));
        assert_eq!(
            resumed.json()["get_updates_buf"],
            "",
            "the cursor was cleared"
        );
        assert_eq!(
            harness.stored(BOT_ID_STORAGE_KEY).await.as_deref(),
            Some("bot-2")
        );
        assert_eq!(
            harness.channel.inbound.context_tokens.get(OWNER).as_deref(),
            Some("ctx"),
            "only the new bot's token is left"
        );
        assert_eq!(harness.state(), ReceiveState::Receiving);
    });
    run(setup(stored, steps), Body);
}
}

rusty_fork_test! {
#[test]
fn a_new_login_for_the_same_bot_keeps_the_cursor() {
    let stored = Stored {
        cursor: Some("cursor-0"),
        bot_id: Some("bot-1"),
        ..Stored::linked().with_owner(OWNER)
    };
    scenario!(Body, |harness| {
        harness
            .channel
            .inbound
            .context_tokens
            .remember(OWNER, "ctx");
        let config = ConfigRequest::from_login("new-token".into(), API_BASE.into());
        assert!(harness.channel.link(config, Some("bot-1")).await.is_ok());

        assert_eq!(*harness.channel.inbound.cursor.borrow(), "cursor-0");
        assert_eq!(
            harness.channel.inbound.context_tokens.get(OWNER).as_deref(),
            Some("ctx")
        );
        assert_eq!(
            harness.stored(CURSOR_STORAGE_KEY).await.as_deref(),
            Some("cursor-0")
        );
    });
    run(setup(stored, Vec::new()), Body);
}
}

rusty_fork_test! {
#[test]
fn disabling_stops_the_loop_frees_the_slot_and_unregisters() {
    let steps = Vec::from([
        updates("cursor-1", Vec::new()),
        hold(),
        updates("cursor-2", Vec::new()),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("receiving", |harness| {
                harness.state() == ReceiveState::Receiving
            })
            .await;
        assert_eq!(harness.in_use(), 1);
        assert!(harness.registered());

        let (code, _body) = harness.set_mode(br#"{"mode":"disabled"}"#).await;
        assert_eq!(code, 204);
        harness.settle().await;

        assert_eq!(harness.state(), ReceiveState::Idle);
        assert_eq!(harness.in_use(), 0);
        assert!(!harness.registered());
        let polls = harness.requests().len();
        assert_eq!(
            harness.stored("mode").await.as_deref(),
            Some("\"disabled\"")
        );

        let (code, body) = harness.set_mode(br#"{"mode":"send"}"#).await;
        assert_eq!((code, body), (400, json!({ "error": "unsupported_mode" })));

        let (code, _body) = harness.set_mode(br#"{"mode":"send_receive"}"#).await;
        assert_eq!(code, 204);
        harness
            .until("polling again", |harness| harness.requests().len() > polls)
            .await;
        assert!(harness.registered());
        assert_eq!(harness.in_use(), 1);
    });
    run(setup(Stored::linked().with_owner(OWNER), steps), Body);
}
}

scenario!(BeforeModes, |harness| {
    assert_eq!(harness.channel.mode(), ChannelMode::SendReceive);
    assert!(harness.registered());
    harness
        .until("the first poll", |harness| harness.requests().len() == 1)
        .await;
});

rusty_fork_test! {
#[test]
fn a_configuration_from_before_modes_receives() {
    run(setup(Stored::linked(), Vec::from([hold()])), BeforeModes);
}

#[test]
fn a_stored_send_mode_receives() {
    let stored = Stored {
        mode: Some("send"),
        ..Stored::linked()
    };
    run(setup(stored, Vec::from([hold()])), BeforeModes);
}
}

rusty_fork_test! {
#[test]
fn without_a_free_slot_it_waits_and_starts_when_one_frees() {
    let steps = Vec::from([updates("cursor-1", Vec::new()), hold()]);
    let mut setup = setup(Stored::linked().with_owner(OWNER), steps);
    setup.hold_slots = true;
    scenario!(Body, |harness| {
        harness.settle().await;
        assert_eq!(harness.state(), ReceiveState::NoSlot);
        assert!(harness.requests().is_empty());
        let capacity = harness.capacity;
        let (_code, body) = harness.status().await;
        assert_eq!(
            body["receive"],
            json!({ "state": "no_slot", "capacity": capacity,
                    "slots": { "in_use": capacity, "capacity": capacity } })
        );
        let (code, body) = harness.set_mode(br#"{"mode":"send_receive"}"#).await;
        assert_eq!(
            (code, body),
            (409, json!({ "error": "no_slot", "capacity": capacity }))
        );

        // The retry timer is an hour away: only the freed slot wakes it.
        harness.release_slots();
        harness
            .until("receiving", |harness| {
                harness.state() == ReceiveState::Receiving
            })
            .await;
        assert_eq!(harness.in_use(), 1);
    });
    run(setup, Body);
}
}

rusty_fork_test! {
#[test]
fn endpoints_report_status_and_owners() {
    let steps = Vec::from([updates("cursor-1", Vec::new()), hold()]);
    scenario!(Body, |harness| {
        harness
            .until("receiving", |harness| {
                harness.state() == ReceiveState::Receiving
            })
            .await;

        let (code, body) = harness.status().await;
        assert_eq!(code, 200);
        assert_eq!(
            body,
            json!({
                "configured": true,
                "mode": "send_receive",
                "receive": { "state": "receiving", "slots": { "in_use": 1, "capacity": harness.capacity } },
                "owners": { "count": 1 },
            })
        );
        assert!(!body.to_string().contains("bot-token"));
        assert_eq!(
            harness.entry_status(),
            EntryStatus::new(
                EntryState::Ready,
                WebText {
                    zh: "收发中",
                    en: "Receiving",
                },
            )
        );

        let (code, body) = harness.owners("GET", b"").await;
        assert_eq!(code, 200);
        assert_eq!(body["owners"], json!([{ "id": OWNER, "label": null }]));
        assert_eq!(body["pairing"]["code"], PAIRING_CODE);
        assert_eq!(body["ignored"], 0);

        let remove = format!(r#"{{"remove":"{OWNER}"}}"#);
        let (code, _body) = harness.owners("POST", remove.as_bytes()).await;
        assert_eq!(code, 204);
        assert_eq!(harness.channel.owners().expect("owners").count(), 0);
    });
    run(setup(Stored::linked().with_owner(OWNER), steps), Body);
}
}

rusty_fork_test! {
#[test]
fn unconfigured_channel_does_not_receive() {
    scenario!(Body, |harness| {
        harness.settle().await;
        assert_eq!(harness.state(), ReceiveState::Idle);
        assert_eq!(harness.in_use(), 0);
        assert!(!harness.registered());
        let (_code, body) = harness.status().await;
        assert_eq!(body["configured"], false);
        assert_eq!(harness.entry_status(), EntryStatus::configured(false));
    });
    run(setup(Stored::default(), Vec::new()), Body);
}
}

/// The key of the test's encrypted files.
const KEY: [u8; 16] = *b"0123456789abcdef";

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nIMAGE";

rusty_fork_test! {
#[test]
fn owner_files_are_decrypted_into_storage_and_missed_ones_become_notes() {
    use base64::Engine as _;
    let raw_key = base64::engine::general_purpose::STANDARD.encode(KEY);
    let hex_key: String = KEY.iter().map(|byte| format!("{byte:02x}")).collect();
    let steps = Vec::from([
        updates(
            "cursor-1",
            Vec::from([
                json!({ "message_id": 10, "from_user_id": OWNER, "message_type": 1,
                        "context_token": "ctx-10",
                        "item_list": [
                            { "type": 1, "text_item": { "text": "look" } },
                            { "type": 2, "image_item": { "aeskey": hex_key,
                              "media": { "encrypt_query_param": "img" } } },
                            { "type": 4, "file_item": { "file_name": "big.zip",
                              "len": "1099511627776",
                              "media": { "encrypt_query_param": "zip", "aes_key": raw_key } } },
                            { "type": 5, "video_item": { "media": { "encrypt_query_param": "vid" } } },
                            { "type": 3, "voice_item": { "encode_type": 6,
                              "media": { "encrypt_query_param": "voice", "aes_key": raw_key } } },
                        ] }),
                json!({ "message_id": 11, "from_user_id": STRANGER, "message_type": 1,
                        "item_list": [{ "type": 2, "image_item": { "aeskey": hex_key,
                          "media": { "encrypt_query_param": "theirs" } } }] }),
                json!({ "message_id": 12, "from_user_id": OWNER, "message_type": 1,
                        "item_list": [{ "type": 4, "file_item": { "file_name": "notes.txt",
                          "len": "5",
                          "media": { "encrypt_query_param": "txt", "aes_key": raw_key } } }] }),
            ]),
        ),
        hold(),
    ]);
    let mut setup = setup(Stored::linked().with_owner(OWNER), steps);
    let image = barracuda_tls::aes_128_ecb_encrypt(&KEY, PNG).expect("encrypt");
    let notes = barracuda_tls::aes_128_ecb_encrypt(&KEY, b"hello").expect("encrypt");
    setup.downloads = Vec::from([
        // Reads of 5 bytes, so blocks arrive split across reads.
        ScriptStep::response(200, "application/octet-stream", &image, 5),
        ScriptStep::json(404, r#"{"error":"gone"}"#),
        ScriptStep::response(200, "application/octet-stream", &notes, 3),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("two published messages", |harness| harness.published().len() == 2)
            .await;
        harness.settle().await;

        assert_eq!(
            harness.published(),
            [
                (
                    OWNER.into(),
                    "10".into(),
                    "look\n[file too large, not received]\n[video not received]\n[audio not received]"
                        .into()
                ),
                (OWNER.into(), "12".into(), "".into()),
            ]
        );
        let attachments = harness.attachments();
        let image = &attachments[0][0];
        assert_eq!(attachments[0].as_array().map(Vec::len), Some(1));
        assert_eq!(image["kind"], "image");
        // The type comes from the decrypted bytes; the length has no padding.
        assert_eq!(image["mime_type"], "image/png");
        assert_eq!(image["size"], PNG.len());
        let path = image["path"].as_str().expect("path");
        assert!(path.starts_with("/workspace/cache/gateway/wechat/") && path.ends_with(".png"));
        let notes = &attachments[1][0];
        assert_eq!(notes["kind"], "file");
        assert_eq!(notes["mime_type"], "text/plain");
        assert_eq!(notes["filename"], "notes.txt");
        assert_eq!(notes["size"], 5);
        let fetched: Vec<String> = harness
            .downloads
            .requests()
            .iter()
            .map(|request| request.lines().next().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            fetched,
            [
                "GET /c2c/download?encrypted_query_param=img HTTP/1.1",
                "GET /c2c/download?encrypted_query_param=voice HTTP/1.1",
                "GET /c2c/download?encrypted_query_param=txt HTTP/1.1",
            ],
            "only owners' files within the limit and with a key are fetched"
        );
        assert!(harness
            .downloads
            .requests()
            .iter()
            .all(|request| !request.contains("bot-token")));
    });
    run(setup, Body);
}
}

//! QQ gateway payloads, message events, and close codes.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

use qq::gateway::{
    close_action, event, heartbeat, identify, op, parse_data, parse_envelope, resume, CloseAction,
    Hello, InboundText, MessageEvent, Ready, Scene, Skipped, BANNED, DELISTED, INTENTS,
};
use serde_json::{json, Value};

#[test]
fn envelopes_and_data_parse_in_two_passes() {
    let hello = r#"{"op":10,"d":{"heartbeat_interval":45000}}"#;
    let envelope = parse_envelope(hello).unwrap();
    assert_eq!(
        (envelope.op, envelope.s, envelope.t),
        (op::HELLO, None, None)
    );
    assert_eq!(
        parse_data::<Hello>(hello).unwrap().heartbeat_interval,
        45000
    );

    let ready = r#"{"op":0,"s":1,"t":"READY","d":{"version":1,"session_id":"082ee18c","user":{"id":"6158","bot":true},"shard":[0,0]}}"#;
    let envelope = parse_envelope(ready).unwrap();
    assert_eq!(envelope.op, op::DISPATCH);
    assert_eq!(envelope.s, Some(1));
    assert_eq!(envelope.t.as_deref(), Some(event::READY));
    assert_eq!(parse_data::<Ready>(ready).unwrap().session_id, "082ee18c");

    let resumed = r#"{"op":0,"s":2002,"t":"RESUMED","d":""}"#;
    assert_eq!(
        parse_envelope(resumed).unwrap().t.as_deref(),
        Some(event::RESUMED)
    );
    let invalid = r#"{"op":9,"d":false}"#;
    assert!(!parse_data::<bool>(invalid).unwrap());
    assert!(parse_envelope("not json").is_err());
}

fn message(value: &Value) -> MessageEvent {
    serde_json::from_value(value.clone()).unwrap()
}

#[test]
fn direct_and_group_messages_name_their_conversation_and_sender() {
    let direct = message(&json!({
        "id": "ROBOT1.0_a",
        "author": {"id": "A1", "user_openid": "A1", "union_openid": "", "username": "", "bot": false},
        "content": "  hello  ",
        "message_type": 0,
        "message_scene": {"source": "default", "ext": ["msg_idx=REFIDX_x=="]},
        "timestamp": "2026-07-21T10:00:00+08:00"
    }));
    assert_eq!(
        direct.into_text(Scene::Direct),
        Ok(InboundText {
            id: "ROBOT1.0_a".into(),
            conversation_id: "c2c:A1".into(),
            sender: "A1".into(),
            label: None,
            text: "hello".into(),
            scene: Scene::Direct,
        })
    );
    let group = message(&json!({
        "id": "ROBOT1.0_b",
        "author": {"id": "M1", "member_openid": "M1", "username": "Ann"},
        "content": " /start 123456",
        "group_openid": "G1",
        "group_id": "G1",
        "timestamp": "2026-07-21T10:00:00+08:00"
    }));
    let group = group.into_text(Scene::Group).unwrap();
    assert_eq!(group.conversation_id, "group:G1");
    assert_eq!(group.sender, "M1");
    assert_eq!(group.label.as_deref(), Some("Ann"));
    assert_eq!(group.text, "/start 123456");
}

#[test]
fn attachments_cards_and_sender_less_events_are_skipped() {
    let image = message(&json!({
        "id": "m", "author": {"user_openid": "A1"}, "content": "",
        "attachments": [{"content_type": "image/png", "url": "https://x"}]
    }));
    assert_eq!(image.into_text(Scene::Direct), Err(Skipped::NotText));
    let card = message(&json!({
        "id": "m", "author": {"user_openid": "A1"}, "content": "[卡片消息]", "message_type": 3
    }));
    assert_eq!(card.into_text(Scene::Direct), Err(Skipped::NotText));
    let no_group = message(&json!({"id": "m", "author": {"member_openid": "M1"}, "content": "hi"}));
    assert_eq!(
        no_group.into_text(Scene::Group),
        Err(Skipped::MissingSender)
    );
    let no_sender = message(&json!({"id": "m", "author": {}, "content": "hi"}));
    assert_eq!(
        no_sender.into_text(Scene::Direct),
        Err(Skipped::MissingSender)
    );
}

#[test]
fn client_payloads_carry_the_token_intents_session_and_sequence() {
    let identify: Value = serde_json::from_str(&identify("tok").unwrap()).unwrap();
    assert_eq!(identify["op"], 2);
    assert_eq!(identify["d"]["token"], "QQBot tok");
    assert_eq!(identify["d"]["intents"], 1 << 25);
    assert_eq!(INTENTS, 33_554_432);
    assert_eq!(identify["d"]["shard"], json!([0, 1]));
    let resume: Value = serde_json::from_str(&resume("tok", "sess", 1337).unwrap()).unwrap();
    assert_eq!(
        resume,
        json!({"op": 6, "d": {"token": "QQBot tok", "session_id": "sess", "seq": 1337}})
    );
    assert_eq!(heartbeat(None).unwrap(), r#"{"op":1,"d":null}"#);
    assert_eq!(heartbeat(Some(251)).unwrap(), r#"{"op":1,"d":251}"#);
}

#[test]
fn close_codes_resume_reidentify_or_halt() {
    assert_eq!(close_action(4009), CloseAction::Resume);
    assert_eq!(close_action(4914), CloseAction::Halt(DELISTED));
    assert_eq!(close_action(4915), CloseAction::Halt(BANNED));
    for code in [1000, 4006, 4007, 4008, 4900, 4913] {
        assert_eq!(close_action(code), CloseAction::Identify, "{code}");
    }
}

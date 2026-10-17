//! Webhook payloads, and webhook registration and the catch-up query against
//! a scripted server, sent through a real Gateway's HTTP.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod bench;

use barracuda_imessage_gateway_plugin::{ChannelError, GatewayHttp, MediaKind};
use barracuda_platform_test::ScriptStep;
use bluebubbles::inbound::{
    is_body_too_large, parse_webhook, MessageQuery, Sort, WebhookPayload, QUERY_BODY_LIMIT,
};
use bluebubbles::{BlueBubbles, BlueBubblesConfig};

use crate::bench::{bench, SERVER_URL};

const MESSAGE: &str = r#"{"originalROWID":12,"guid":"p:0/AB-12","text":"hello","attributedBody":[{"string":"hello","runs":[]}],"handle":{"originalROWID":3,"address":"+15550001111","service":"iMessage","country":"us","uncanonicalizedId":null},"handleId":3,"otherHandle":0,"attachments":[],"subject":null,"error":0,"dateCreated":1700000000123,"dateRead":null,"isFromMe":false,"itemType":0,"associatedMessageGuid":null,"associatedMessageType":null,"chats":[{"originalROWID":2,"guid":"iMessage;-;+15550001111","style":45,"chatIdentifier":"+15550001111","isArchived":false,"displayName":"","participants":[{"address":"+15550001111"}]},{"guid":"second"}]}"#;

fn provider(http: GatewayHttp) -> BlueBubbles {
    BlueBubbles::new(http, BlueBubblesConfig::new(SERVER_URL, "pw"))
}

#[test]
fn a_new_message_webhook_keeps_only_the_fields_used() {
    let body = format!(r#"{{"type":"new-message","data":{MESSAGE}}}"#);
    let WebhookPayload::NewMessage(message) = parse_webhook(body.as_bytes()).expect("payload")
    else {
        panic!("not a new message");
    };
    assert_eq!(message.guid, "p:0/AB-12");
    assert_eq!(message.body().as_deref(), Some("hello"));
    assert_eq!(message.attachments().count(), 0);
    assert!(!message.is_from_me);
    assert_eq!(message.sender(), Some("+15550001111"));
    assert_eq!(message.chat_guid(), Some("iMessage;-;+15550001111"));
    assert_eq!(message.date_created, Some(1_700_000_000_123));
}

#[test]
fn other_events_and_garbage_are_told_apart() {
    assert_eq!(
        parse_webhook(br#"{"type":"typing-indicator","data":{"display":true}}"#),
        Ok(WebhookPayload::Other)
    );
    assert!(parse_webhook(b"{}").is_err());
    assert!(parse_webhook(br#"{"type":"new-message","data":{"text":"no guid"}}"#).is_err());
}

#[test]
fn reactions_and_group_events_are_no_messages() {
    let parse = |replace: (&str, &str)| {
        let body = format!(
            r#"{{"type":"new-message","data":{}}}"#,
            MESSAGE.replace(replace.0, replace.1)
        );
        match parse_webhook(body.as_bytes()).expect("payload") {
            WebhookPayload::NewMessage(message) => message,
            WebhookPayload::Other => panic!("not a new message"),
        }
    };
    let reaction = parse((
        r#""associatedMessageType":null"#,
        r#""associatedMessageType":"love""#,
    ));
    assert_eq!(reaction.body(), None);
    let group_event = parse((r#""itemType":0"#, r#""itemType":2"#));
    assert_eq!(group_event.body(), None);
    let marks_only = parse((r#""text":"hello""#, "\"text\":\"\u{fffc} \""));
    assert_eq!(marks_only.body().as_deref(), Some(""));
    let no_text = parse((r#""text":"hello""#, r#""text":null"#));
    assert_eq!(no_text.body().as_deref(), Some(""));
    let no_chat = parse((r#""chats":[{"#, r#""chats":[],"x":[{"#));
    assert_eq!(no_chat.chat_guid(), None);
}

#[test]
fn attachments_keep_their_guid_type_name_and_size_and_marks_leave_the_text() {
    let attachments = r#""attachments":[
        {"originalROWID":1,"guid":"at_0_A","uti":"public.jpeg","mimeType":"image/jpeg","transferName":"IMG_1.jpeg","totalBytes":1234,"height":10,"width":10},
        {"guid":"at_1_B","mimeType":"image/heic","transferName":"IMG_2.HEIC","totalBytes":99},
        {"guid":"at_2_C","mimeType":"audio/x-caf","transferName":"Audio Message.caf"},
        {"guid":"at_3_D","mimeType":"video/quicktime","transferName":"clip.mov"},
        {"guid":"at_4_E","mimeType":null,"transferName":"notes.pages"},
        {"guid":"at_5_F","mimeType":null,"transferName":"9D1E.pluginPayloadAttachment"}
    ]"#;
    let body = format!(
        r#"{{"type":"new-message","data":{}}}"#,
        MESSAGE
            .replace(r#""attachments":[]"#, attachments)
            .replace(r#""text":"hello""#, "\"text\":\"\u{fffc}look \u{fffc}\"")
    );
    let WebhookPayload::NewMessage(message) = parse_webhook(body.as_bytes()).expect("payload")
    else {
        panic!("not a new message");
    };
    assert_eq!(message.body().as_deref(), Some("look"));
    let attachments: Vec<_> = message.attachments().collect();
    assert_eq!(attachments.len(), 5, "the link preview is no attachment");
    let first = attachments[0];
    assert_eq!(first.guid, "at_0_A");
    assert_eq!(first.mime(), Some("image/jpeg"));
    assert_eq!(first.name(), Some("IMG_1.jpeg"));
    assert_eq!(first.total_bytes, Some(1234));
    let kinds: Vec<_> = attachments
        .iter()
        .map(|attachment| attachment.kind())
        .collect();
    assert_eq!(
        kinds,
        [
            MediaKind::Image,
            MediaKind::File,
            MediaKind::Audio,
            MediaKind::Video,
            MediaKind::File
        ]
    );
    assert_eq!(attachments[4].mime(), None);

    let null = format!(
        r#"{{"type":"new-message","data":{}}}"#,
        MESSAGE.replace(r#""attachments":[]"#, r#""attachments":null"#)
    );
    let WebhookPayload::NewMessage(message) = parse_webhook(null.as_bytes()).expect("payload")
    else {
        panic!("not a new message");
    };
    assert_eq!(message.attachments().count(), 0);
}

#[test]
fn an_attachment_downloads_from_its_guid_with_the_password() {
    bench([], async |http, _server| {
        let provider = BlueBubbles::new(http, BlueBubblesConfig::new(SERVER_URL, "p w&"));
        assert_eq!(
            provider.attachment_url("at_0_A/B"),
            format!("{SERVER_URL}/api/v1/attachment/at_0_A%2FB/download?password=p%20w%26")
        );
    });
}

#[test]
fn webhooks_are_listed_created_and_deleted() {
    let steps = [
        ScriptStep::json(
            200,
            r#"{"status":200,"message":"ok","data":[{"id":4,"url":"http://10.0.0.2:8787/hook/x","events":["new-message"],"created":"2026-01-01"}]}"#,
        ),
        ScriptStep::json(
            200,
            r#"{"status":200,"message":"ok","data":{"id":5,"url":"http://10.0.0.2:8787/hook/y","events":["new-message"]}}"#,
        ),
        ScriptStep::json(
            404,
            r#"{"status":404,"message":"Not Found","error":{"type":"Not Found","message":"Webhook does not exist!"}}"#,
        ),
        ScriptStep::json(401, r#"{"status":401,"message":"Unauthorized"}"#),
    ];
    bench(steps, async |http, server| {
        let provider = provider(http);

        let listed = provider.list_webhooks().await.expect("list");
        let created = provider
            .create_webhook("http://10.0.0.2:8787/hook/y")
            .await
            .expect("create");
        let gone = provider.delete_webhook(9).await;
        let refused = provider.list_webhooks().await;

        assert_eq!(listed.len(), 1);
        assert_eq!(
            (listed[0].id, listed[0].url.as_str()),
            (4, "http://10.0.0.2:8787/hook/x")
        );
        assert_eq!(created.id, 5);
        assert_eq!(gone, Ok(()), "an unknown ID counts as deleted");
        assert!(matches!(refused, Err(ChannelError::Authentication)));
        let requests = server.requests();
        assert!(requests[0].starts_with("GET /api/v1/webhook?password=pw "));
        assert!(requests[1].starts_with("POST /api/v1/webhook?password=pw "));
        assert!(requests[1]
            .ends_with(r#"{"events":["new-message"],"url":"http://10.0.0.2:8787/hook/y"}"#));
        assert!(requests[2].starts_with("DELETE /api/v1/webhook/9?password=pw "));
    });
}

#[test]
fn the_message_query_is_parsed_and_capped() {
    let page = format!(
        r#"{{"status":200,"message":"ok","data":[{MESSAGE}],"metadata":{{"offset":0,"limit":10,"total":31,"count":1}}}}"#
    );
    let huge = format!(
        r#"{{"status":200,"data":[{{"guid":"x","text":"{}"}}]}}"#,
        "y".repeat(QUERY_BODY_LIMIT)
    );
    let steps = [ScriptStep::json(200, &page), ScriptStep::json(200, &huge)];
    bench(steps, async |http, server| {
        let provider = provider(http);
        let query = MessageQuery {
            after: Some(1_700_000_000_000),
            offset: 10,
            limit: 10,
            sort: Sort::Ascending,
        };

        let page = provider.query_messages(query).await.expect("page");
        let too_large = provider.query_messages(query).await.expect_err("too large");

        assert_eq!(page.total, Some(31));
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].guid, "p:0/AB-12");
        assert!(is_body_too_large(&too_large));
        let request = &server.requests()[0];
        assert!(request.starts_with("POST /api/v1/message/query?password=pw "));
        assert!(request.ends_with(
            r#"{"after":1700000000000,"limit":10,"offset":10,"sort":"ASC","with":["chat","attachment"]}"#
        ));
    });
}

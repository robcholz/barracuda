//! Webhook payloads, webhook registration, and the catch-up query.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::boxed::Box;

use barracuda_imessage_gateway_plugin::ChannelError;
use barracuda_platform_test::{ScriptStep, ScriptedStack};
use bluebubbles::inbound::{
    is_body_too_large, parse_webhook, MessageQuery, Sort, WebhookPayload, QUERY_BODY_LIMIT,
};
use bluebubbles::{BlueBubbles, BlueBubblesConfig};
use futures_lite::future::block_on;
use http_client::ClientFactory;

const MESSAGE: &str = r#"{"originalROWID":12,"guid":"p:0/AB-12","text":"hello","attributedBody":[{"string":"hello","runs":[]}],"handle":{"originalROWID":3,"address":"+15550001111","service":"iMessage","country":"us","uncanonicalizedId":null},"handleId":3,"otherHandle":0,"attachments":[],"subject":null,"error":0,"dateCreated":1700000000123,"dateRead":null,"isFromMe":false,"itemType":0,"associatedMessageGuid":null,"associatedMessageType":null,"chats":[{"originalROWID":2,"guid":"iMessage;-;+15550001111","style":45,"chatIdentifier":"+15550001111","isArchived":false,"displayName":"","participants":[{"address":"+15550001111"}]},{"guid":"second"}]}"#;

fn server(steps: impl IntoIterator<Item = ScriptStep>) -> &'static ScriptedStack {
    Box::leak(Box::new(ScriptedStack::new(steps)))
}

fn provider(server: &'static ScriptedStack) -> BlueBubbles<'static, ScriptedStack, ScriptedStack> {
    BlueBubbles::new(
        ClientFactory::from_network(server, server),
        BlueBubblesConfig::new("http://blue.test", "pw"),
    )
}

#[test]
fn a_new_message_webhook_keeps_only_the_fields_used() {
    let body = format!(r#"{{"type":"new-message","data":{MESSAGE}}}"#);
    let WebhookPayload::NewMessage(message) = parse_webhook(body.as_bytes()).expect("payload")
    else {
        panic!("not a new message");
    };
    assert_eq!(message.guid, "p:0/AB-12");
    assert_eq!(message.plain_text(), Some("hello"));
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
fn only_ordinary_text_counts_as_text() {
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
    assert_eq!(reaction.plain_text(), None);
    let group_event = parse((r#""itemType":0"#, r#""itemType":2"#));
    assert_eq!(group_event.plain_text(), None);
    let attachment = parse((r#""text":"hello""#, "\"text\":\"\u{fffc} \""));
    assert_eq!(attachment.plain_text(), None);
    let no_text = parse((r#""text":"hello""#, r#""text":null"#));
    assert_eq!(no_text.plain_text(), None);
    let no_chat = parse((r#""chats":[{"#, r#""chats":[],"x":[{"#));
    assert_eq!(no_chat.chat_guid(), None);
}

#[test]
fn webhooks_are_listed_created_and_deleted() {
    let server = server([
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
    ]);
    let provider = provider(server);

    let listed = block_on(provider.list_webhooks()).expect("list");
    let created = block_on(provider.create_webhook("http://10.0.0.2:8787/hook/y")).expect("create");
    let gone = block_on(provider.delete_webhook(9));
    let refused = block_on(provider.list_webhooks());

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
    assert!(
        requests[1].ends_with(r#"{"events":["new-message"],"url":"http://10.0.0.2:8787/hook/y"}"#)
    );
    assert!(requests[2].starts_with("DELETE /api/v1/webhook/9?password=pw "));
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
    let server = server([ScriptStep::json(200, &page), ScriptStep::json(200, &huge)]);
    let provider = provider(server);
    let query = MessageQuery {
        after: Some(1_700_000_000_000),
        offset: 10,
        limit: 10,
        sort: Sort::Ascending,
    };

    let page = block_on(provider.query_messages(query)).expect("page");
    let too_large = block_on(provider.query_messages(query)).expect_err("too large");

    assert_eq!(page.total, Some(31));
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].guid, "p:0/AB-12");
    assert!(is_body_too_large(&too_large));
    let request = &server.requests()[0];
    assert!(request.starts_with("POST /api/v1/message/query?password=pw "));
    assert!(request.ends_with(
        r#"{"after":1700000000000,"limit":10,"offset":10,"sort":"ASC","with":["chat"]}"#
    ));
}

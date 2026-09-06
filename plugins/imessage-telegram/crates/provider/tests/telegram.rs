#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::{boxed::Box, rc::Rc};

use barracuda_imessage_gateway_plugin::{
    BinaryBody, BinaryChunk, DeleteMessageRequest, EditMessageRequest, MediaKind, MessageChannel,
    MessageTarget, ReactRequest, SendMediaRequest, SendMessageRequest, SetTypingRequest, TextChunk,
};
use barracuda_platform_test::{ScriptStep, ScriptedStack};
use futures_lite::{future::block_on, stream};
use http_client::ClientFactory;
use telegram::{Telegram, TelegramConfig};

struct Header {
    name: String,
    value: String,
}

impl Header {
    fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

struct Response {
    status: u16,
    body: Vec<u8>,
}

struct MockHttp {
    network: &'static ScriptedStack,
}

impl MockHttp {
    fn responding(responses: impl IntoIterator<Item = Response>) -> Self {
        let steps = responses.into_iter().map(|response| {
            ScriptStep::response(
                response.status,
                "application/json",
                &response.body,
                usize::MAX,
            )
        });
        Self {
            network: Box::leak(Box::new(ScriptedStack::new(steps))),
        }
    }

    fn factory(&self) -> ClientFactory<'static, ScriptedStack, ScriptedStack> {
        ClientFactory::from_network(self.network, self.network)
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.network
            .requests()
            .into_iter()
            .map(RecordedRequest::parse)
            .collect()
    }
}

struct RecordedRequest {
    url: String,
    headers: Vec<Header>,
    body: Vec<u8>,
}

impl RecordedRequest {
    fn parse(raw: String) -> Self {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let mut lines = head.split("\r\n");
        let url = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or_default()
            .to_owned();
        let headers = lines
            .filter_map(|line| line.split_once(":"))
            .map(|(name, value)| Header::new(name, value.trim_start()))
            .collect();
        Self {
            url,
            headers,
            body: body.as_bytes().to_vec(),
        }
    }
}

fn response(body: &str) -> Response {
    Response {
        status: 200,
        body: body.as_bytes().to_vec(),
    }
}

fn target() -> MessageTarget {
    MessageTarget::new("telegram", "42")
}

fn config(token: &str) -> TelegramConfig {
    let mut config = TelegramConfig::new(token);
    config.api_base = "http://api.telegram.test".to_owned();
    config
}

fn body_json(request: &RecordedRequest) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null)
}

#[test]
fn registers_as_the_telegram_channel_and_sends_text() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response(
            r#"{"ok":true,"result":{"message_id":7}}"#,
        )]));
        let channel = Telegram::new(http.factory(), config("bot-token"));

        let receipt = channel
            .send_message(SendMessageRequest::text(target(), "hello"))
            .await;

        assert_eq!(channel.channel(), "telegram");
        assert!(matches!(receipt, Ok(receipt) if receipt.message_id == "7"));
        let requests = http.requests();
        let Some(request) = requests.first() else {
            panic!("missing request");
        };
        assert_eq!(request.url, "/botbot-token/sendMessage");
        assert_eq!(body_json(request)["chat_id"], 42);
        assert_eq!(body_json(request)["text"], "hello");
    });
}

#[test]
fn maps_text_chunks_to_drafts_and_finishes_with_a_normal_message() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            response(r#"{"ok":true,"result":true}"#),
            response(r#"{"ok":true,"result":true}"#),
            response(r#"{"ok":true,"result":{"message_id":9}}"#),
        ]));
        let mut settings = config("token");
        settings.draft_min_delta_bytes = 1;
        let channel = Telegram::new(http.factory(), settings);
        let chunks = stream::iter([
            Ok(TextChunk::inline("hel").expect("chunk fits inline storage")),
            Ok(TextChunk::inline("lo").expect("chunk fits inline storage")),
        ]);

        let result = channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await;

        assert!(matches!(result, Ok(receipt) if receipt.message_id == "9"));
        let requests = http.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests
            .first()
            .is_some_and(|request| request.url.ends_with("/sendMessageDraft")));
        assert_eq!(
            requests.first().map(body_json).unwrap_or_default()["text"],
            "hel"
        );
        assert_eq!(
            requests.get(1).map(body_json).unwrap_or_default()["text"],
            "hello"
        );
        assert!(requests
            .get(2)
            .is_some_and(|request| request.url.ends_with("/sendMessage")));
    });
}

#[test]
fn sends_images_as_multipart_without_buffering_stream_inputs() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response(
            r#"{"ok":true,"result":{"message_id":11}}"#,
        )]));
        let channel = Telegram::new(http.factory(), config("token"));
        let data = stream::iter([
            Ok(BinaryChunk::from(vec![1, 2])),
            Ok(BinaryChunk::from(vec![3, 4])),
        ]);
        let request = SendMediaRequest {
            target: target(),
            body: BinaryBody::Stream(Box::pin(data)),
            filename: Some("photo.jpg".to_owned()),
            mime_type: Some("image/jpeg".to_owned()),
            caption: Some("caption".to_owned()),
            reply_to: None,
        };

        let result = channel.send_media(MediaKind::Image, request).await;

        assert!(matches!(result, Ok(receipt) if receipt.message_id == "11"));
        let requests = http.requests();
        let Some(request) = requests.first() else {
            panic!("missing request");
        };
        assert!(request.url.ends_with("/sendPhoto"));
        assert!(request.headers.iter().any(|header| {
            header.name == "Content-Type"
                && header.value.starts_with("multipart/form-data; boundary=")
        }));
        assert!(request
            .headers
            .iter()
            .any(|header| { header.name == "Transfer-Encoding" && header.value == "chunked" }));
    });
}

#[test]
fn maps_message_mutations_reactions_and_typing() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            response(r#"{"ok":true,"result":{"message_id":7}}"#),
            response(r#"{"ok":true,"result":true}"#),
            response(r#"{"ok":true,"result":true}"#),
            response(r#"{"ok":true,"result":true}"#),
        ]));
        let channel = Telegram::new(http.factory(), config("token"));

        channel
            .edit_message(EditMessageRequest::new(target(), "7", "edited"))
            .await
            .expect("edit succeeds");
        channel
            .delete_message(DeleteMessageRequest::new(target(), "7"))
            .await
            .expect("delete succeeds");
        channel
            .react(ReactRequest::new(target(), "7", "👍"))
            .await
            .expect("reaction succeeds");
        channel
            .set_typing(SetTypingRequest::new(target(), true))
            .await
            .expect("typing succeeds");
        channel
            .set_typing(SetTypingRequest::new(target(), false))
            .await
            .expect("stopping typing is local");

        let requests = http.requests();
        let methods: Vec<&str> = requests
            .iter()
            .filter_map(|request| request.url.rsplit('/').next())
            .collect();
        assert_eq!(
            methods,
            [
                "editMessageText",
                "deleteMessage",
                "setMessageReaction",
                "sendChatAction"
            ]
        );
        assert_eq!(body_json(&requests[0])["message_id"], 7);
        assert_eq!(body_json(&requests[0])["text"], "edited");
        assert_eq!(body_json(&requests[2])["reaction"][0]["emoji"], "👍");
        assert_eq!(body_json(&requests[3])["action"], "typing");
    });
}

#[test]
fn maps_telegram_authentication_and_rate_limit_errors() {
    block_on(async {
        for (status, expected_auth) in [(401, true), (429, false)] {
            let http = Rc::new(MockHttp::responding([Response {
                status,
                body: br#"{"ok":false}"#.to_vec(),
            }]));
            let channel = Telegram::new(http.factory(), config("token"));
            let error = channel
                .send_message(SendMessageRequest::text(target(), "hello"))
                .await
                .expect_err("request should fail");
            assert_eq!(
                matches!(
                    &error,
                    barracuda_imessage_gateway_plugin::ChannelError::Authentication
                ),
                expected_auth
            );
            assert_eq!(
                matches!(
                    &error,
                    barracuda_imessage_gateway_plugin::ChannelError::RateLimited
                ),
                !expected_auth
            );
        }
    });
}

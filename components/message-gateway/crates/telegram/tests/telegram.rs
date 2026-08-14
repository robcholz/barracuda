#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use futures_lite::{future::block_on, stream};
use gateway::{
    BinaryBody, DeleteMessageRequest, EditMessageRequest, MediaKind, MessageChannel, MessageTarget,
    ReactRequest, SendMediaRequest, SendMessageRequest, SetTypingRequest,
};
use http_client::{Body, Error, HttpClient, HttpFuture, Request, Response};
use telegram::{Telegram, TelegramConfig};

#[derive(Default)]
struct MockHttp {
    requests: RefCell<Vec<Request>>,
    responses: RefCell<VecDeque<Response>>,
}

impl MockHttp {
    fn responding(responses: impl IntoIterator<Item = Response>) -> Self {
        Self {
            requests: RefCell::new(Vec::new()),
            responses: RefCell::new(responses.into_iter().collect()),
        }
    }
}

impl HttpClient for MockHttp {
    fn execute(&self, request: Request) -> HttpFuture<'_> {
        Box::pin(async move {
            self.requests.borrow_mut().push(request);
            self.responses
                .borrow_mut()
                .pop_front()
                .ok_or(Error::ConnectionAborted)
        })
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

fn body_json(request: &Request) -> serde_json::Value {
    let Body::Bytes(bytes) = &request.body else {
        return serde_json::Value::Null;
    };
    serde_json::from_slice(bytes).unwrap_or(serde_json::Value::Null)
}

#[test]
fn registers_as_the_telegram_channel_and_sends_text() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response(
            r#"{"ok":true,"result":{"message_id":7}}"#,
        )]));
        let channel = Telegram::new(
            Rc::clone(&http) as Rc<dyn HttpClient>,
            TelegramConfig::new("bot-token"),
        );

        let receipt = channel
            .send_message(SendMessageRequest::text(target(), "hello"))
            .await;

        assert_eq!(channel.channel(), "telegram");
        assert!(matches!(receipt, Ok(receipt) if receipt.message_id == "7"));
        let requests = http.requests.borrow();
        let Some(request) = requests.first() else {
            panic!("missing request");
        };
        assert_eq!(
            request.url,
            "https://api.telegram.org/botbot-token/sendMessage"
        );
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
        let mut config = TelegramConfig::new("token");
        config.draft_min_delta_bytes = 1;
        let channel = Telegram::new(Rc::clone(&http) as Rc<dyn HttpClient>, config);
        let chunks = stream::iter([Ok("hel".to_owned()), Ok("lo".to_owned())]);

        let result = channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await;

        assert!(matches!(result, Ok(receipt) if receipt.message_id == "9"));
        let requests = http.requests.borrow();
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
        let channel = Telegram::new(
            Rc::clone(&http) as Rc<dyn HttpClient>,
            TelegramConfig::new("token"),
        );
        let data = stream::iter([Ok(vec![1, 2]), Ok(vec![3, 4])]);
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
        let requests = http.requests.borrow();
        let Some(request) = requests.first() else {
            panic!("missing request");
        };
        assert!(request.url.ends_with("/sendPhoto"));
        assert!(request.headers.iter().any(|header| {
            header.name == "Content-Type"
                && header.value.starts_with("multipart/form-data; boundary=")
        }));
        assert!(matches!(request.body, Body::Stream { .. }));
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
        let channel = Telegram::new(
            Rc::clone(&http) as Rc<dyn HttpClient>,
            TelegramConfig::new("token"),
        );

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

        let requests = http.requests.borrow();
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
            let channel = Telegram::new(
                Rc::clone(&http) as Rc<dyn HttpClient>,
                TelegramConfig::new("token"),
            );
            let error = channel
                .send_message(SendMessageRequest::text(target(), "hello"))
                .await
                .expect_err("request should fail");
            assert_eq!(
                matches!(&error, gateway::ChannelError::Authentication),
                expected_auth
            );
            assert_eq!(
                matches!(&error, gateway::ChannelError::RateLimited),
                !expected_auth
            );
        }
    });
}

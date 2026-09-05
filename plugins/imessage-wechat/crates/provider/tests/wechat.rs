#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::{boxed::Box, rc::Rc};

use barracuda_platform_test::{ScriptStep, ScriptedStack};
use futures_lite::{future::block_on, stream};
use gateway::{MessageChannel, MessageTarget, SendMessageRequest, TextChunk};
use http_client::ClientFactory;
use wechat::{Wechat, WechatConfig};

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

struct MockHttp {
    network: &'static ScriptedStack,
}

impl MockHttp {
    fn responding(count: usize) -> Self {
        let steps = (0..count).map(|_| ScriptStep::json(200, r#"{"ret":0}"#));
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

fn target() -> MessageTarget {
    let mut target = MessageTarget::new("wechat", "wx-user");
    target.thread_id = Some("context-token".to_owned());
    target
}

fn body_json(request: &RecordedRequest) -> serde_json::Value {
    serde_json::from_slice(&request.body).expect("valid JSON request")
}

fn config(token: &str) -> WechatConfig {
    let mut config = WechatConfig::new(token);
    config.api_base = "http://wechat.test".to_owned();
    config
}

#[test]
fn registers_and_maps_text_to_the_ilink_api() {
    block_on(async {
        let http = Rc::new(MockHttp::responding(1));
        let channel = Wechat::new(http.factory(), config("secret-token"));

        let receipt = channel
            .send_message(SendMessageRequest::text(target(), "hello"))
            .await
            .expect("send succeeds");

        assert_eq!(channel.channel(), "wechat");
        assert!(receipt.message_id.starts_with("espwx-"));
        let requests = http.requests();
        let request = requests.first().expect("one request");
        assert_eq!(request.url, "/ilink/bot/sendmessage");
        assert!(request.headers.iter().any(|header| {
            header.name == "Authorization" && header.value == "Bearer secret-token"
        }));
        assert!(request.headers.iter().any(|header| {
            header.name == "AuthorizationType" && header.value == "ilink_bot_token"
        }));
        let json = body_json(request);
        assert_eq!(json["msg"]["to_user_id"], "wx-user");
        assert_eq!(json["msg"]["context_token"], "context-token");
        assert_eq!(json["msg"]["item_list"][0]["text_item"]["text"], "hello");
        assert_eq!(json["base_info"]["channel_version"], "barracuda-wechat");
    });
}

#[test]
fn buffers_an_async_text_stream_into_one_wechat_message() {
    block_on(async {
        let http = Rc::new(MockHttp::responding(1));
        let channel = Wechat::new(http.factory(), config("token"));
        let chunks = stream::iter([Ok(TextChunk::from("hel")), Ok(TextChunk::from("lo"))]);

        channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await
            .expect("send succeeds");

        let requests = http.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            body_json(requests.first().expect("request"))["msg"]["item_list"][0]["text_item"]
                ["text"],
            "hello"
        );
    });
}

#[test]
fn splits_long_text_only_on_utf8_boundaries() {
    block_on(async {
        let http = Rc::new(MockHttp::responding(2));
        let channel = Wechat::new(http.factory(), config("token"));
        let text = format!("{}好", "a".repeat(3999));

        channel
            .send_message(SendMessageRequest::text(target(), text))
            .await
            .expect("send succeeds");

        let requests = http.requests();
        assert_eq!(requests.len(), 2);
        let first = body_json(requests.first().expect("first"));
        let second = body_json(requests.get(1).expect("second"));
        assert_eq!(
            first["msg"]["item_list"][0]["text_item"]["text"],
            "a".repeat(3999)
        );
        assert_eq!(second["msg"]["item_list"][0]["text_item"]["text"], "好");
    });
}

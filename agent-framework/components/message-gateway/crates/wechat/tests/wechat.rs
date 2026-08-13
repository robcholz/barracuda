#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use futures_lite::{future::block_on, stream};
use gateway::{MessageChannel, MessageTarget, SendMessageRequest};
use http_client::{Body, Error, HttpClient, HttpFuture, Request, Response};
use wechat::{Wechat, WechatConfig};

#[derive(Default)]
struct MockHttp {
    requests: RefCell<Vec<Request>>,
    responses: RefCell<VecDeque<Response>>,
}

impl MockHttp {
    fn responding(count: usize) -> Self {
        Self {
            requests: RefCell::new(Vec::new()),
            responses: RefCell::new(
                (0..count)
                    .map(|_| Response {
                        status: 200,
                        body: br#"{"ret":0}"#.to_vec(),
                    })
                    .collect(),
            ),
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

fn target() -> MessageTarget {
    let mut target = MessageTarget::new("wechat", "wx-user");
    target.thread_id = Some("context-token".to_owned());
    target
}

fn body_json(request: &Request) -> serde_json::Value {
    let Body::Bytes(bytes) = &request.body else {
        panic!("expected JSON bytes");
    };
    serde_json::from_slice(bytes).expect("valid JSON request")
}

#[test]
fn registers_and_maps_text_to_the_ilink_api() {
    block_on(async {
        let http = Rc::new(MockHttp::responding(1));
        let channel = Wechat::new(
            Rc::clone(&http) as Rc<dyn HttpClient>,
            WechatConfig::new("secret-token"),
        );

        let receipt = channel
            .send_message(SendMessageRequest::text(target(), "hello"))
            .await
            .expect("send succeeds");

        assert_eq!(channel.channel(), "wechat");
        assert!(receipt.message_id.starts_with("espwx-"));
        let requests = http.requests.borrow();
        let request = requests.first().expect("one request");
        assert_eq!(
            request.url,
            "https://ilinkai.weixin.qq.com/ilink/bot/sendmessage"
        );
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
        assert_eq!(json["base_info"]["channel_version"], "esp-claw-wechat");
    });
}

#[test]
fn buffers_an_async_text_stream_into_one_wechat_message() {
    block_on(async {
        let http = Rc::new(MockHttp::responding(1));
        let channel = Wechat::new(
            Rc::clone(&http) as Rc<dyn HttpClient>,
            WechatConfig::new("token"),
        );
        let chunks = stream::iter([Ok("hel".to_owned()), Ok("lo".to_owned())]);

        channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await
            .expect("send succeeds");

        let requests = http.requests.borrow();
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
        let channel = Wechat::new(
            Rc::clone(&http) as Rc<dyn HttpClient>,
            WechatConfig::new("token"),
        );
        let text = format!("{}好", "a".repeat(3999));

        channel
            .send_message(SendMessageRequest::text(target(), text))
            .await
            .expect("send succeeds");

        let requests = http.requests.borrow();
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

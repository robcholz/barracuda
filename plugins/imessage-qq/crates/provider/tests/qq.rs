#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use barracuda_platform_test::{ScriptStep, ScriptedStack};
use futures_lite::{future::block_on, stream};
use gateway::{MessageChannel, MessageTarget, SendMessageRequest, StreamError};
use http_client::ClientFactory;
use qq::{QQConfig, QQ};
use std::{boxed::Box, rc::Rc};

struct MockHttp {
    network: &'static ScriptedStack,
}
impl MockHttp {
    fn responding(count: usize) -> Self {
        Self {
            network: Box::leak(Box::new(ScriptedStack::new(
                (0..count).map(|_| ScriptStep::json(200, r#"{"id":"qq-message-1"}"#)),
            ))),
        }
    }
    fn scripted(steps: impl IntoIterator<Item = ScriptStep>) -> Self {
        Self {
            network: Box::leak(Box::new(ScriptedStack::new(steps))),
        }
    }
    fn factory(&self) -> ClientFactory<'static, ScriptedStack, ScriptedStack> {
        ClientFactory::from_network(self.network, self.network)
    }
}
fn provider(http: &MockHttp) -> QQ<'static, ScriptedStack, ScriptedStack> {
    let mut config = QQConfig::new("app-123", "secret-token");
    config.api_base = "http://qq.test".to_owned();
    QQ::new(http.factory(), config)
}

#[test]
fn maps_c2c_text_to_qq_bot_api() {
    block_on(async {
        let http = Rc::new(MockHttp::responding(1));
        let channel = provider(&http);
        let request =
            SendMessageRequest::text(MessageTarget::new("qq", "c2c:user-openid"), "hello");
        let receipt = channel.send_message(request).await.expect("send succeeds");
        assert_eq!(channel.channel(), "qq");
        assert_eq!(receipt.message_id, "qq-message-1");
        let raw = http.network.requests().pop().expect("request");
        assert!(raw.starts_with("POST /v2/users/user-openid/messages HTTP/1.1"));
        assert!(raw.contains("Authorization: QQBot secret-token"));
        assert!(raw.contains("X-Union-Appid: app-123"));
        assert!(raw.contains(r#""content":"hello""#));
        assert!(raw.contains(r#""msg_type":0"#));
    });
}

#[test]
fn maps_group_channel_and_stream_destinations() {
    block_on(async {
        let http = Rc::new(MockHttp::responding(2));
        let channel = provider(&http);
        channel
            .send_message(SendMessageRequest::text(
                MessageTarget::new("qq", "group:group-openid"),
                "group",
            ))
            .await
            .expect("group send");
        let chunks = stream::iter([Ok("hel".to_owned()), Ok("lo".to_owned())]);
        channel
            .send_message(SendMessageRequest::stream(
                MessageTarget::new("qq", "channel:42"),
                Box::pin(chunks),
            ))
            .await
            .expect("channel send");
        let requests = http.network.requests();
        assert!(requests[0].starts_with("POST /v2/groups/group-openid/messages HTTP/1.1"));
        assert!(requests[1].starts_with("POST /channels/42/messages HTTP/1.1"));
        assert!(requests[1].contains(r#""content":"hello""#));
        assert!(!requests[1].contains("msg_type"));
    });
}

#[test]
fn rejects_untyped_or_unsafe_destination() {
    block_on(async {
        let http = Rc::new(MockHttp::responding(0));
        let channel = provider(&http);
        for conversation in ["123", "c2c:", "group:a/b", "channel:a?b", "c2c:a#b"] {
            let result = channel
                .send_message(SendMessageRequest::text(
                    MessageTarget::new("qq", conversation),
                    "hello",
                ))
                .await;
            assert!(result.is_err());
        }
        assert!(http.network.requests().is_empty());
    });
}

#[test]
fn replies_and_stream_failures_have_explicit_wire_behavior() {
    block_on(async {
        let http = Rc::new(MockHttp::responding(1));
        let channel = provider(&http);
        let mut reply = SendMessageRequest::text(MessageTarget::new("qq", "channel:42"), "reply");
        reply.reply_to = Some("original".into());
        channel.send_message(reply).await.expect("reply sends");
        assert!(http.network.requests()[0].contains(r#""msg_id":"original""#));

        let no_network = Rc::new(MockHttp::responding(0));
        let channel = provider(&no_network);
        assert!(channel
            .send_message(SendMessageRequest::text(
                MessageTarget::new("qq", "c2c:user"),
                "",
            ))
            .await
            .is_err());
        let chunks = stream::iter([
            Ok("partial".to_owned()),
            Err(StreamError::failed("stopped")),
        ]);
        assert!(channel
            .send_message(SendMessageRequest::stream(
                MessageTarget::new("qq", "c2c:user"),
                Box::pin(chunks),
            ))
            .await
            .is_err());
        assert!(no_network.network.requests().is_empty());
    });
}

#[test]
fn maps_auth_rate_limit_platform_malformed_and_incomplete_responses() {
    block_on(async {
        for step in [
            ScriptStep::json(401, "{}"),
            ScriptStep::json(403, "{}"),
            ScriptStep::json(429, "{}"),
            ScriptStep::json(500, r#"{"code":77,"message":"bad"}"#),
            ScriptStep::json(500, "{}"),
            ScriptStep::json(200, "not-json"),
            ScriptStep::json(200, "{}"),
        ] {
            let http = MockHttp::scripted([step]);
            assert!(provider(&http)
                .send_message(SendMessageRequest::text(
                    MessageTarget::new("qq", "c2c:user"),
                    "hello",
                ))
                .await
                .is_err());
        }
    });
}

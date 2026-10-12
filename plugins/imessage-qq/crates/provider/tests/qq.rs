#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! The QQ provider against a scripted QQ server, sending through a real
//! Gateway's HTTP on a loopback network.

mod support;

use barracuda_imessage_gateway_plugin::{
    ChannelError, GatewayHttp, MessageChannel, MessageTarget, SendMessageRequest, TextChunk,
};
use barracuda_platform_test::{ScriptStep, ScriptedServer};
use futures_lite::{future::zip, stream};
use qq::{QQConfig, TokenError, QQ};
use std::boxed::Box;
use support::{bench, ORIGIN};

const TOKEN: &str = r#"{"access_token":"token-1","expires_in":"7200"}"#;
const MESSAGE: &str = r#"{"id":"qq-message-1"}"#;
const TOKEN_REQUEST: &str = "POST /app/getAppAccessToken HTTP/1.1";

/// A token, then `count` sent messages.
fn responding(count: usize) -> impl Iterator<Item = ScriptStep> {
    core::iter::once(ScriptStep::json(200, TOKEN))
        .chain((0..count).map(|_| ScriptStep::json(200, MESSAGE)))
}
fn token_requests(server: &ScriptedServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|request| request.starts_with(TOKEN_REQUEST))
        .count()
}
fn config() -> QQConfig {
    let mut config = QQConfig::new("app-123", "app-secret");
    config.api_base = ORIGIN.to_owned();
    config.token_url = format!("{ORIGIN}/app/getAppAccessToken");
    config
}
fn provider(http: GatewayHttp) -> QQ {
    QQ::new(http, config())
}
fn c2c(text: &str) -> SendMessageRequest {
    SendMessageRequest::text(MessageTarget::new("qq", "c2c:user-openid"), text)
}

#[test]
fn fetches_access_token_before_first_send() {
    bench(responding(1), async |http, server| {
        let channel = provider(http);
        let request = c2c("hello");
        let receipt = channel.send_message(request).await.expect("send succeeds");
        assert_eq!(channel.channel(), "qq");
        assert_eq!(receipt.message_id, "qq-message-1");
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with(TOKEN_REQUEST));
        assert!(requests[0].contains(r#""appId":"app-123""#));
        assert!(requests[0].contains(r#""clientSecret":"app-secret""#));
        assert!(!requests[0].contains("Authorization"));
        let raw = &requests[1];
        assert!(raw.starts_with("POST /v2/users/user-openid/messages HTTP/1.1"));
        assert!(raw.contains("Authorization: QQBot token-1"));
        assert!(raw.contains("X-Union-Appid: app-123"));
        assert!(raw.contains(r#""content":"hello""#));
        assert!(raw.contains(r#""msg_type":0"#));
        assert!(!raw.contains("app-secret"));
    });
}

#[test]
fn maps_group_channel_and_stream_destinations_with_one_cached_token() {
    bench(responding(2), async |http, server| {
        let channel = provider(http);
        channel
            .send_message(SendMessageRequest::text(
                MessageTarget::new("qq", "group:group-openid"),
                "group",
            ))
            .await
            .expect("group send");
        let chunks = stream::iter([
            Ok(TextChunk::inline("hel").expect("chunk fits inline storage")),
            Ok(TextChunk::inline("lo").expect("chunk fits inline storage")),
        ]);
        channel
            .send_message(SendMessageRequest::stream(
                MessageTarget::new("qq", "channel:42"),
                Box::pin(chunks),
            ))
            .await
            .expect("channel send");
        let requests = server.requests();
        assert_eq!(token_requests(server), 1);
        assert!(requests[1].starts_with("POST /v2/groups/group-openid/messages HTTP/1.1"));
        assert!(requests[2].starts_with("POST /channels/42/messages HTTP/1.1"));
        assert!(requests[2].contains("Authorization: QQBot token-1"));
        assert!(requests[2].contains(r#""content":"hello""#));
        assert!(!requests[2].contains("msg_type"));
    });
}

#[test]
fn authenticated_token_is_reused_by_the_first_send() {
    bench(responding(1), async |http, server| {
        let channel = provider(http);
        channel.authenticate().await.expect("credentials accepted");
        channel.send_message(c2c("hello")).await.expect("send");
        assert_eq!(token_requests(server), 1);
        assert_eq!(server.requests().len(), 2);
    });
}

#[test]
fn refreshes_token_within_sixty_seconds_of_expiry() {
    bench(
        [
            ScriptStep::json(200, r#"{"access_token":"token-1","expires_in":30}"#),
            ScriptStep::json(200, MESSAGE),
            ScriptStep::json(200, r#"{"access_token":"token-2","expires_in":"7200"}"#),
            ScriptStep::json(200, MESSAGE),
            ScriptStep::json(200, MESSAGE),
        ],
        async |http, server| {
            let channel = provider(http);
            for _ in 0..3 {
                channel.send_message(c2c("hello")).await.expect("send");
            }
            let requests = server.requests();
            assert_eq!(requests.len(), 5);
            assert!(requests[1].contains("Authorization: QQBot token-1"));
            assert!(requests[2].starts_with(TOKEN_REQUEST));
            assert!(requests[3].contains("Authorization: QQBot token-2"));
            assert!(requests[4].contains("Authorization: QQBot token-2"));
        },
    );
}

#[test]
fn unauthorized_send_refreshes_once_and_retries_once() {
    bench(
        [
            ScriptStep::json(200, TOKEN),
            ScriptStep::json(
                401,
                r#"{"code":11244,"message":"token not exist or expire"}"#,
            ),
            ScriptStep::json(200, r#"{"access_token":"token-2","expires_in":"7200"}"#),
            ScriptStep::json(200, MESSAGE),
        ],
        async |http, server| {
            let channel = provider(http);
            let receipt = channel.send_message(c2c("hello")).await.expect("retry");
            assert_eq!(receipt.message_id, "qq-message-1");
            let requests = server.requests();
            assert_eq!(requests.len(), 4);
            assert!(requests[1].contains("Authorization: QQBot token-1"));
            assert!(requests[2].starts_with(TOKEN_REQUEST));
            assert!(requests[3].contains("Authorization: QQBot token-2"));
            assert!(requests[3].contains(r#""content":"hello""#));
        },
    );
}

#[test]
fn repeated_unauthorized_send_stops_after_one_retry() {
    bench(
        [
            ScriptStep::json(200, TOKEN),
            ScriptStep::json(401, "{}"),
            ScriptStep::json(200, r#"{"access_token":"token-2","expires_in":"7200"}"#),
            ScriptStep::json(401, "{}"),
            ScriptStep::json(200, MESSAGE),
        ],
        async |http, server| {
            let channel = provider(http);
            let result = channel.send_message(c2c("hello")).await;
            assert_eq!(
                result.expect_err("still unauthorized"),
                ChannelError::Authentication
            );
            assert_eq!(server.requests().len(), 4);
            assert_eq!(server.remaining(), 1);
        },
    );
}

#[test]
fn concurrent_sends_share_one_token_refresh() {
    bench(responding(2), async |http, server| {
        let channel = provider(http);
        let (first, second) = zip(
            channel.send_message(c2c("first")),
            channel.send_message(c2c("second")),
        )
        .await;
        first.expect("first send");
        second.expect("second send");
        assert_eq!(token_requests(server), 1);
        assert_eq!(server.requests().len(), 3);
    });
}

#[test]
fn rejected_app_secret_reports_qq_code_and_message() {
    bench(
        [
            ScriptStep::json(200, r#"{"code":10004,"message":"机器人不存在"}"#),
            ScriptStep::json(200, r#"{"code":10004,"message":"机器人不存在"}"#),
        ],
        async |http, server| {
            let channel = provider(http);
            assert_eq!(
                channel.authenticate().await,
                Err(TokenError::Rejected {
                    code: Some("10004".into()),
                    message: Some("机器人不存在".into()),
                })
            );
            let result = channel.send_message(c2c("hello")).await;
            assert_eq!(
                result.expect_err("token rejected"),
                ChannelError::Platform {
                    code: Some("10004".into()),
                    message: "机器人不存在".into(),
                }
            );
            assert_eq!(token_requests(server), 2);
            assert_eq!(server.requests().len(), 2);
        },
    );
}

#[test]
fn unreachable_or_non_json_token_endpoint_is_unavailable() {
    bench(
        [
            ScriptStep::ConnectError(embedded_io::ErrorKind::ConnectionRefused),
            ScriptStep::response(502, "text/html", b"<html>bad gateway</html>", usize::MAX),
        ],
        async |http, server| {
            let channel = provider(http);
            for _ in 0..2 {
                assert!(matches!(
                    channel.authenticate().await,
                    Err(TokenError::Unavailable { .. })
                ));
            }
            assert_eq!(server.remaining(), 0);
        },
    );
}

#[test]
fn rejects_untyped_or_unsafe_destination() {
    bench([], async |http, server| {
        let channel = provider(http);
        for conversation in ["123", "c2c:", "group:a/b"] {
            let result = channel
                .send_message(SendMessageRequest::text(
                    MessageTarget::new("qq", conversation),
                    "hello",
                ))
                .await;
            assert!(result.is_err());
        }
        assert!(server.requests().is_empty());
    });
}

#[test]
fn gateway_url_is_fetched_with_the_cached_token() {
    bench(
        [
            ScriptStep::json(200, TOKEN),
            ScriptStep::json(401, "{}"),
            ScriptStep::json(200, r#"{"access_token":"token-2","expires_in":"7200"}"#),
            ScriptStep::json(200, r#"{"url":"wss://api.sgroup.qq.com/websocket/"}"#),
            ScriptStep::json(200, r#"{"code":100001,"message":"rate limited"}"#),
        ],
        async |http, server| {
            let channel = provider(http);
            assert_eq!(
                channel.gateway_url().await.expect("gateway url"),
                "wss://api.sgroup.qq.com/websocket/"
            );
            let requests = server.requests();
            assert!(requests[1].starts_with("GET /gateway HTTP/1.1"));
            assert!(requests[1].contains("Authorization: QQBot token-1"));
            assert!(requests[3].contains("Authorization: QQBot token-2"));
            assert!(channel.token_refresh_at().is_some());
            assert!(matches!(
                channel.gateway_url().await,
                Err(ChannelError::Platform { .. })
            ));
        },
    );
}

#[test]
fn replies_to_a_received_message_carry_msg_id_and_an_increasing_msg_seq() {
    bench(responding(3), async |http, server| {
        let channel = provider(http);
        channel.note_inbound("ROBOT1.0_in", qq::gateway::Scene::Direct);
        for _ in 0..2 {
            let mut request = c2c("reply");
            request.reply_to = Some("ROBOT1.0_in".into());
            channel.send_message(request).await.expect("reply");
        }
        let mut request = SendMessageRequest::text(MessageTarget::new("qq", "channel:42"), "x");
        request.reply_to = Some("guild-msg".into());
        channel.send_message(request).await.expect("guild reply");
        let requests = server.requests();
        assert!(requests[1].contains(r#""msg_id":"ROBOT1.0_in""#));
        assert!(requests[1].contains(r#""msg_seq":1"#));
        assert!(requests[2].contains(r#""msg_seq":2"#));
        assert!(requests[3].contains(r#""msg_id":"guild-msg""#));
        assert!(!requests[3].contains("msg_seq"));
    });
}

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use barracuda_imessage_gateway_plugin::{
    ChannelError, MessageChannel, MessageTarget, SendMessageRequest, TextChunk,
};
use barracuda_platform_test::{ScriptError, ScriptStep, ScriptedConnection, ScriptedStack};
use futures_lite::{
    future::{block_on, yield_now, zip},
    stream,
};
use http_client::embedded_nal_async::{AddrType, Dns, TcpConnect};
use http_client::ClientFactory;
use qq::{QQConfig, TokenError, QQ};
use std::{
    boxed::Box,
    net::{IpAddr, SocketAddr},
    rc::Rc,
};

const TOKEN: &str = r#"{"access_token":"token-1","expires_in":"7200"}"#;
const MESSAGE: &str = r#"{"id":"qq-message-1"}"#;
const TOKEN_REQUEST: &str = "POST /app/getAppAccessToken HTTP/1.1";

struct MockHttp {
    network: &'static ScriptedStack,
}
impl MockHttp {
    fn scripted(steps: impl IntoIterator<Item = ScriptStep>) -> Self {
        Self {
            network: Box::leak(Box::new(ScriptedStack::new(steps))),
        }
    }
    fn responding(count: usize) -> Self {
        Self::scripted(
            core::iter::once(ScriptStep::json(200, TOKEN))
                .chain((0..count).map(|_| ScriptStep::json(200, MESSAGE))),
        )
    }
    fn factory(&self) -> ClientFactory<'static, ScriptedStack, ScriptedStack> {
        ClientFactory::from_network(self.network, self.network)
    }
    fn token_requests(&self) -> usize {
        self.network
            .requests()
            .iter()
            .filter(|request| request.starts_with(TOKEN_REQUEST))
            .count()
    }
}
fn config() -> QQConfig {
    let mut config = QQConfig::new("app-123", "app-secret");
    config.api_base = "http://qq.test".to_owned();
    config.token_url = "http://qq.test/app/getAppAccessToken".to_owned();
    config
}
fn provider(http: &MockHttp) -> QQ<'static, ScriptedStack, ScriptedStack> {
    QQ::new(http.factory(), config())
}
fn c2c(text: &str) -> SendMessageRequest {
    SendMessageRequest::text(MessageTarget::new("qq", "c2c:user-openid"), text)
}

/// Scripted network that yields before every connection, so concurrent sends interleave.
struct YieldingStack(&'static ScriptedStack);
impl Dns for YieldingStack {
    type Error = ScriptError;

    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        self.0.get_host_by_name(host, addr_type).await
    }

    async fn get_host_by_address(
        &self,
        addr: IpAddr,
        result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        self.0.get_host_by_address(addr, result).await
    }
}
impl TcpConnect for YieldingStack {
    type Error = ScriptError;
    type Connection<'a> = ScriptedConnection;

    async fn connect<'a>(
        &'a self,
        remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        yield_now().await;
        self.0.connect(remote).await
    }
}

#[test]
fn fetches_access_token_before_first_send() {
    block_on(async {
        let http = Rc::new(MockHttp::responding(1));
        let channel = provider(&http);
        let request = c2c("hello");
        let receipt = channel.send_message(request).await.expect("send succeeds");
        assert_eq!(channel.channel(), "qq");
        assert_eq!(receipt.message_id, "qq-message-1");
        let requests = http.network.requests();
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
        let requests = http.network.requests();
        assert_eq!(http.token_requests(), 1);
        assert!(requests[1].starts_with("POST /v2/groups/group-openid/messages HTTP/1.1"));
        assert!(requests[2].starts_with("POST /channels/42/messages HTTP/1.1"));
        assert!(requests[2].contains("Authorization: QQBot token-1"));
        assert!(requests[2].contains(r#""content":"hello""#));
        assert!(!requests[2].contains("msg_type"));
    });
}

#[test]
fn authenticated_token_is_reused_by_the_first_send() {
    block_on(async {
        let http = MockHttp::responding(1);
        let channel = provider(&http);
        channel.authenticate().await.expect("credentials accepted");
        channel.send_message(c2c("hello")).await.expect("send");
        assert_eq!(http.token_requests(), 1);
        assert_eq!(http.network.requests().len(), 2);
    });
}

#[test]
fn refreshes_token_within_sixty_seconds_of_expiry() {
    block_on(async {
        let http = MockHttp::scripted([
            ScriptStep::json(200, r#"{"access_token":"token-1","expires_in":30}"#),
            ScriptStep::json(200, MESSAGE),
            ScriptStep::json(200, r#"{"access_token":"token-2","expires_in":"7200"}"#),
            ScriptStep::json(200, MESSAGE),
            ScriptStep::json(200, MESSAGE),
        ]);
        let channel = provider(&http);
        for _ in 0..3 {
            channel.send_message(c2c("hello")).await.expect("send");
        }
        let requests = http.network.requests();
        assert_eq!(requests.len(), 5);
        assert!(requests[1].contains("Authorization: QQBot token-1"));
        assert!(requests[2].starts_with(TOKEN_REQUEST));
        assert!(requests[3].contains("Authorization: QQBot token-2"));
        assert!(requests[4].contains("Authorization: QQBot token-2"));
    });
}

#[test]
fn unauthorized_send_refreshes_once_and_retries_once() {
    block_on(async {
        let http = MockHttp::scripted([
            ScriptStep::json(200, TOKEN),
            ScriptStep::json(
                401,
                r#"{"code":11244,"message":"token not exist or expire"}"#,
            ),
            ScriptStep::json(200, r#"{"access_token":"token-2","expires_in":"7200"}"#),
            ScriptStep::json(200, MESSAGE),
        ]);
        let channel = provider(&http);
        let receipt = channel.send_message(c2c("hello")).await.expect("retry");
        assert_eq!(receipt.message_id, "qq-message-1");
        let requests = http.network.requests();
        assert_eq!(requests.len(), 4);
        assert!(requests[1].contains("Authorization: QQBot token-1"));
        assert!(requests[2].starts_with(TOKEN_REQUEST));
        assert!(requests[3].contains("Authorization: QQBot token-2"));
        assert!(requests[3].contains(r#""content":"hello""#));
    });
}

#[test]
fn repeated_unauthorized_send_stops_after_one_retry() {
    block_on(async {
        let http = MockHttp::scripted([
            ScriptStep::json(200, TOKEN),
            ScriptStep::json(401, "{}"),
            ScriptStep::json(200, r#"{"access_token":"token-2","expires_in":"7200"}"#),
            ScriptStep::json(401, "{}"),
            ScriptStep::json(200, MESSAGE),
        ]);
        let channel = provider(&http);
        let result = channel.send_message(c2c("hello")).await;
        assert_eq!(
            result.expect_err("still unauthorized"),
            ChannelError::Authentication
        );
        assert_eq!(http.network.requests().len(), 4);
        assert_eq!(http.network.remaining(), 1);
    });
}

#[test]
fn concurrent_sends_share_one_token_refresh() {
    block_on(async {
        let http = MockHttp::responding(2);
        let network: &'static YieldingStack = Box::leak(Box::new(YieldingStack(http.network)));
        let channel = QQ::new(ClientFactory::from_network(network, network), config());
        let (first, second) = zip(
            channel.send_message(c2c("first")),
            channel.send_message(c2c("second")),
        )
        .await;
        first.expect("first send");
        second.expect("second send");
        assert_eq!(http.token_requests(), 1);
        assert_eq!(http.network.requests().len(), 3);
    });
}

#[test]
fn rejected_app_secret_reports_qq_code_and_message() {
    block_on(async {
        let http = MockHttp::scripted([
            ScriptStep::json(200, r#"{"code":10004,"message":"机器人不存在"}"#),
            ScriptStep::json(200, r#"{"code":10004,"message":"机器人不存在"}"#),
        ]);
        let channel = provider(&http);
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
        assert_eq!(http.token_requests(), 2);
        assert_eq!(http.network.requests().len(), 2);
    });
}

#[test]
fn unreachable_or_non_json_token_endpoint_is_unavailable() {
    block_on(async {
        let http = MockHttp::scripted([
            ScriptStep::ConnectError(embedded_io::ErrorKind::ConnectionRefused),
            ScriptStep::response(502, "text/html", b"<html>bad gateway</html>", usize::MAX),
        ]);
        let channel = provider(&http);
        for _ in 0..2 {
            assert!(matches!(
                channel.authenticate().await,
                Err(TokenError::Unavailable { .. })
            ));
        }
    });
}

#[test]
fn rejects_untyped_or_unsafe_destination() {
    block_on(async {
        let http = Rc::new(MockHttp::scripted([]));
        let channel = provider(&http);
        for conversation in ["123", "c2c:", "group:a/b"] {
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

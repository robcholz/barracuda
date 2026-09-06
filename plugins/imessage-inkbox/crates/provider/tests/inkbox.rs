#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::{boxed::Box, rc::Rc};

use barracuda_imessage_gateway_plugin::{
    BinaryBody, MediaKind, MessageChannel, MessageTarget, ReactRequest, SendMediaRequest,
    SendMessageRequest, SetTypingRequest, TextChunk,
};
use barracuda_platform_test::{ScriptStep, ScriptedStack};
use futures_lite::{future::block_on, stream};
use http_client::ClientFactory;
use inkbox::{Inkbox, InkboxConfig};

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
    MessageTarget::new("imessage", "conversation-uuid")
}

fn text_chunk(text: &str) -> TextChunk {
    TextChunk::inline(text).expect("test text chunk fits inline storage")
}

fn provider(http: &Rc<MockHttp>) -> Inkbox<'static, ScriptedStack, ScriptedStack> {
    let mut config = InkboxConfig::new("ApiKey_secret", "identity-uuid");
    config.api_base = "http://inkbox.test".to_owned();
    Inkbox::new(http.factory(), config)
}

fn body_json(request: &RecordedRequest) -> serde_json::Value {
    serde_json::from_slice(&request.body).expect("valid JSON")
}

#[test]
fn registers_as_imessage_and_sends_text_for_the_configured_identity() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response(
            r#"{"message":{"id":"message-uuid"}}"#,
        )]));
        let channel = provider(&http);
        let receipt = channel
            .send_message(SendMessageRequest::text(target(), "hello"))
            .await
            .expect("send succeeds");

        assert_eq!(channel.channel(), "imessage");
        assert_eq!(receipt.message_id, "message-uuid");
        let requests = http.requests();
        let request = requests.first().expect("request");
        assert_eq!(
            request.url,
            "/api/v1/imessage/messages?agent_identity_id=identity-uuid"
        );
        assert!(request
            .headers
            .iter()
            .any(|header| header.name == "X-API-Key" && header.value == "ApiKey_secret"));
        assert_eq!(body_json(request)["conversation_id"], "conversation-uuid");
        assert_eq!(body_json(request)["text"], "hello");
    });
}

#[test]
fn buffers_streams_and_maps_tapbacks_and_typing() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            response(r#"{"message":{"id":"stream-id"}}"#),
            response(r#"{"id":"reaction-id"}"#),
            response(r#"{}"#),
        ]));
        let channel = provider(&http);
        let chunks = stream::iter([Ok(text_chunk("hel")), Ok(text_chunk("lo"))]);
        channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await
            .expect("stream succeeds");
        channel
            .react(ReactRequest::new(target(), "inbound-id", "👍"))
            .await
            .expect("reaction succeeds");
        channel
            .set_typing(SetTypingRequest::new(target(), true))
            .await
            .expect("typing succeeds");
        channel
            .set_typing(SetTypingRequest::new(target(), false))
            .await
            .expect("typing stop is local");

        let requests = http.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(body_json(&requests[0])["text"], "hello");
        assert_eq!(body_json(&requests[1])["reaction"], "like");
        assert_eq!(
            body_json(&requests[2])["conversation_id"],
            "conversation-uuid"
        );
    });
}

#[test]
fn uploads_media_then_sends_the_returned_url() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            response(r#"{"media_url":"https://media.example/photo.jpg"}"#),
            response(r#"{"message":{"id":"media-id"}}"#),
        ]));
        let channel = provider(&http);
        let request = SendMediaRequest {
            target: target(),
            body: BinaryBody::Bytes(vec![1, 2, 3]),
            filename: Some("photo.jpg".to_owned()),
            mime_type: Some("image/jpeg".to_owned()),
            caption: Some("caption".to_owned()),
            reply_to: None,
        };
        let receipt = channel
            .send_media(MediaKind::Image, request)
            .await
            .expect("media succeeds");

        assert_eq!(receipt.message_id, "media-id");
        let requests = http.requests();
        assert!(requests[0].url.ends_with("/imessage/media"));
        assert!(requests[0].headers.iter().any(|header| {
            header.name == "Content-Type"
                && header.value.starts_with("multipart/form-data; boundary=")
        }));
        assert_eq!(
            body_json(&requests[1])["media_urls"][0],
            "https://media.example/photo.jpg"
        );
        assert_eq!(body_json(&requests[1])["text"], "caption");
    });
}

#[test]
fn maps_authentication_and_rate_limits() {
    block_on(async {
        for (status, authentication) in [(401, true), (429, false)] {
            let http = Rc::new(MockHttp::responding([Response {
                status,
                body: br#"{"detail":"failed"}"#.to_vec(),
            }]));
            let error = provider(&http)
                .send_message(SendMessageRequest::text(target(), "hello"))
                .await
                .expect_err("fails");
            assert_eq!(
                matches!(
                    error,
                    barracuda_imessage_gateway_plugin::ChannelError::Authentication
                ),
                authentication
            );
            assert_eq!(
                matches!(
                    error,
                    barracuda_imessage_gateway_plugin::ChannelError::RateLimited
                ),
                !authentication
            );
        }
    });
}

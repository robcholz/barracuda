#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::{boxed::Box, rc::Rc};

use barracuda_platform_test::{ScriptStep, ScriptedStack};
use futures_lite::{future::block_on, stream};
use gateway::{
    BinaryBody, MediaKind, MessageChannel, MessageTarget, ReactRequest, SendMediaRequest,
    SendMessageRequest, SetTypingRequest, StreamError,
};
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
        let chunks = stream::iter([Ok("hel".to_owned()), Ok("lo".to_owned())]);
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
                matches!(error, gateway::ChannelError::Authentication),
                authentication
            );
            assert_eq!(
                matches!(error, gateway::ChannelError::RateLimited),
                !authentication
            );
        }
    });
}

#[test]
fn rejects_unsupported_request_shapes_and_failed_text_streams_without_network_io() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([]));
        let channel = provider(&http);

        assert!(matches!(
            channel
                .send_message(SendMessageRequest::text(target(), ""))
                .await,
            Err(gateway::ChannelError::InvalidRequest { .. })
        ));
        let mut reply = SendMessageRequest::text(target(), "hello");
        reply.reply_to = Some("message-1".into());
        assert!(matches!(
            channel.send_message(reply).await,
            Err(gateway::ChannelError::InvalidRequest { .. })
        ));
        let failed = stream::iter([Err(StreamError::failed("source stopped"))]);
        assert!(matches!(
            channel
                .send_message(SendMessageRequest::stream(target(), Box::pin(failed)))
                .await,
            Err(gateway::ChannelError::Stream(_))
        ));
        assert!(matches!(
            channel
                .react(ReactRequest::new(target(), "message-1", "unknown"))
                .await,
            Err(gateway::ChannelError::InvalidRequest { .. })
        ));
        let mut media =
            SendMediaRequest::bytes(target(), "file.bin", "application/octet-stream", vec![1]);
        media.reply_to = Some("message-1".into());
        assert!(matches!(
            channel.send_media(MediaKind::File, media).await,
            Err(gateway::ChannelError::InvalidRequest { .. })
        ));
        channel
            .set_typing(SetTypingRequest::new(target(), false))
            .await
            .expect("typing stop is local");
        assert!(http.requests().is_empty());
    });
}

#[test]
fn every_supported_tapback_maps_to_the_inkbox_reaction_name() {
    block_on(async {
        let reactions = [
            ("❤️", "love"),
            ("like", "like"),
            ("👎", "dislike"),
            ("laugh", "laugh"),
            ("‼️", "emphasize"),
            ("question", "question"),
            ("👀", "eyes"),
        ];
        let http = Rc::new(MockHttp::responding(
            reactions.iter().map(|_| response(r#"{}"#)),
        ));
        let channel = provider(&http);
        for (input, _expected) in reactions {
            channel
                .react(ReactRequest::new(target(), "message-1", input))
                .await
                .expect("supported reaction");
        }
        let requests = http.requests();
        assert_eq!(requests.len(), reactions.len());
        for (request, (_input, expected)) in requests.iter().zip(reactions) {
            assert_eq!(body_json(request)["reaction"], expected);
            assert_eq!(body_json(request)["part_index"], 0);
        }
    });
}

#[test]
fn platform_response_errors_and_default_media_metadata_remain_explicit() {
    block_on(async {
        for response in [
            Response {
                status: 403,
                body: br#"{}"#.to_vec(),
            },
            Response {
                status: 500,
                body: br#"{"detail":"specific detail"}"#.to_vec(),
            },
            Response {
                status: 502,
                body: br#"{"message":"specific message"}"#.to_vec(),
            },
            Response {
                status: 503,
                body: br#"{}"#.to_vec(),
            },
            Response {
                status: 200,
                body: b"not-json".to_vec(),
            },
            response(r#"{"message":{}}"#),
        ] {
            let http = Rc::new(MockHttp::responding([response]));
            assert!(provider(&http)
                .send_message(SendMessageRequest::text(target(), "hello"))
                .await
                .is_err());
        }

        let http = Rc::new(MockHttp::responding([response(r#"{}"#)]));
        let media = SendMediaRequest {
            target: target(),
            body: BinaryBody::Bytes(vec![1, 2]),
            filename: None,
            mime_type: None,
            caption: None,
            reply_to: None,
        };
        assert!(matches!(
            provider(&http).send_media(MediaKind::Audio, media).await,
            Err(gateway::ChannelError::Platform { .. })
        ));
        let requests = http.requests();
        let upload = requests.first().expect("upload request");
        let body = String::from_utf8_lossy(&upload.body);
        assert!(body.contains("filename=\"audio.bin\""));
        assert!(body.contains("Content-Type: application/octet-stream"));
    });
}

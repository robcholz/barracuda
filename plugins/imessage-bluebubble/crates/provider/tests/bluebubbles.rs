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
use bluebubbles::{BlueBubbles, BlueBubblesConfig};
use futures_lite::{future::block_on, stream};
use http_client::ClientFactory;

#[derive(Debug, Eq, PartialEq)]
enum Method {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

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

impl Default for MockHttp {
    fn default() -> Self {
        Self::responding([])
    }
}

struct RecordedRequest {
    method: Method,
    url: String,
    headers: Vec<Header>,
    body: Vec<u8>,
}

impl RecordedRequest {
    fn parse(raw: String) -> Self {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let mut lines = head.split("\r\n");
        let mut request_line = lines.next().unwrap_or_default().split_whitespace();
        let method = match request_line.next().unwrap_or_default() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            "PATCH" => Method::Patch,
            other => panic!("unexpected HTTP method {other}"),
        };
        let url = request_line.next().unwrap_or_default().to_owned();
        let headers = lines
            .filter_map(|line| line.split_once(":"))
            .map(|(name, value)| Header::new(name, value.trim_start()))
            .collect();
        Self {
            method,
            url,
            headers,
            body: body.as_bytes().to_vec(),
        }
    }
}

fn response(guid: &str) -> Response {
    Response {
        status: 200,
        body: format!(r#"{{"status":200,"message":"Success","data":{{"guid":"{guid}"}}}}"#)
            .into_bytes(),
    }
}

fn target() -> MessageTarget {
    MessageTarget::new("bluebubbles", "iMessage;-;+15551234567")
}

fn text_chunk(text: &str) -> TextChunk {
    TextChunk::inline(text).expect("test text chunk fits inline storage")
}

fn binary_chunk(bytes: &[u8]) -> BinaryChunk {
    let mut chunk = BinaryChunk::empty_inline();
    for &byte in bytes {
        assert!(chunk.push(byte));
    }
    chunk
}

fn body_json(request: &RecordedRequest) -> serde_json::Value {
    serde_json::from_slice(&request.body).expect("valid JSON request")
}

fn provider(http: &Rc<MockHttp>) -> BlueBubbles<'static, ScriptedStack, ScriptedStack> {
    BlueBubbles::new(
        http.factory(),
        BlueBubblesConfig::new("http://mac.local:1234", "p@ss word&"),
    )
}

#[test]
fn registers_as_bluebubbles_and_sends_replies_with_encoded_auth() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response("message-guid")]));
        let channel = provider(&http);
        let mut request = SendMessageRequest::text(target(), "hello");
        request.reply_to = Some("parent-guid".to_owned());

        let receipt = channel.send_message(request).await.expect("send succeeds");

        assert_eq!(channel.channel(), "bluebubbles");
        assert_eq!(receipt.message_id, "message-guid");
        let requests = http.requests();
        let request = requests.first().expect("one request");
        assert_eq!(
            request.url,
            "/api/v1/message/text?password=p%40ss%20word%26"
        );
        let json = body_json(request);
        assert_eq!(json["chatGuid"], "iMessage;-;+15551234567");
        assert_eq!(json["message"], "hello");
        assert_eq!(json["method"], "private-api");
        assert_eq!(json["selectedMessageGuid"], "parent-guid");
        assert!(json["tempGuid"]
            .as_str()
            .is_some_and(|id| id.starts_with("temp-barracuda-")));
    });
}

#[test]
fn streams_by_sending_once_then_editing_the_same_message() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            response("stream-guid"),
            response("stream-guid"),
        ]));
        let channel = provider(&http);
        let chunks = stream::iter([Ok(text_chunk("hel")), Ok(text_chunk("lo"))]);

        let receipt = channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await
            .expect("stream succeeds");

        assert_eq!(receipt.message_id, "stream-guid");
        let requests = http.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.contains("/api/v1/message/text?"));
        assert!(requests[1]
            .url
            .contains("/api/v1/message/stream-guid/edit?"));
        assert_eq!(body_json(&requests[0])["message"], "hel");
        assert_eq!(body_json(&requests[1])["editedMessage"], "hello");
        assert_eq!(
            body_json(&requests[1])["backwardsCompatibilityMessage"],
            "hello"
        );
    });
}

#[test]
fn sends_streamed_attachments_without_buffering_them() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response("attachment-guid")]));
        let channel = provider(&http);
        let data = stream::iter([Ok(binary_chunk(&[1, 2])), Ok(binary_chunk(&[3, 4]))]);
        let request = SendMediaRequest {
            target: target(),
            body: BinaryBody::Stream(Box::pin(data)),
            filename: Some("voice.m4a".to_owned()),
            mime_type: Some("audio/mp4".to_owned()),
            caption: Some("voice note".to_owned()),
            reply_to: Some("parent-guid".to_owned()),
        };

        let receipt = channel
            .send_media(MediaKind::Audio, request)
            .await
            .expect("attachment succeeds");

        assert_eq!(receipt.message_id, "attachment-guid");
        let requests = http.requests();
        let request = requests.first().expect("one request");
        assert!(request.url.contains("/api/v1/message/attachment?"));
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
fn maps_edit_unsend_reaction_and_typing_endpoints() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            response("message-guid"),
            response("message-guid"),
            response("reaction-guid"),
            Response {
                status: 200,
                body: br#"{"status":200,"message":"Success"}"#.to_vec(),
            },
            Response {
                status: 200,
                body: br#"{"status":200,"message":"Success"}"#.to_vec(),
            },
        ]));
        let channel = provider(&http);

        channel
            .edit_message(EditMessageRequest::new(target(), "message/guid", "edited"))
            .await
            .expect("edit succeeds");
        channel
            .delete_message(DeleteMessageRequest::new(target(), "message/guid"))
            .await
            .expect("unsend succeeds");
        channel
            .react(ReactRequest::new(target(), "message/guid", "❤️"))
            .await
            .expect("reaction succeeds");
        channel
            .set_typing(SetTypingRequest::new(target(), true))
            .await
            .expect("start typing succeeds");
        channel
            .set_typing(SetTypingRequest::new(target(), false))
            .await
            .expect("stop typing succeeds");

        let requests = http.requests();
        assert_eq!(requests.len(), 5);
        assert!(requests[0].url.contains("/message/message%2Fguid/edit?"));
        assert!(requests[1].url.contains("/message/message%2Fguid/unsend?"));
        assert!(requests[2].url.contains("/message/react?"));
        assert_eq!(body_json(&requests[2])["reaction"], "love");
        assert!(requests[3]
            .url
            .contains("/chat/iMessage%3B-%3B%2B15551234567/typing?"));
        assert_eq!(requests[3].method, Method::Post);
        assert_eq!(requests[4].method, Method::Delete);
    });
}

#[test]
fn without_private_api_streaming_falls_back_and_mutations_are_unsupported() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response("message-guid")]));
        let mut config = BlueBubblesConfig::new("http://mac.local", "password");
        config.use_private_api = false;
        let channel = BlueBubbles::new(http.factory(), config);
        let chunks = stream::iter([Ok(text_chunk("hel")), Ok(text_chunk("lo"))]);

        channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await
            .expect("fallback send succeeds");
        let edit = channel
            .edit_message(EditMessageRequest::new(target(), "guid", "text"))
            .await;

        assert!(matches!(
            edit,
            Err(barracuda_imessage_gateway_plugin::ChannelError::Unsupported { .. })
        ));
        let requests = http.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(body_json(&requests[0])["message"], "hello");
        assert_eq!(body_json(&requests[0])["method"], "apple-script");
    });
}

#[test]
fn without_private_api_replies_are_sent_unthreaded() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            response("text-guid"),
            response("stream-guid"),
            response("attachment-guid"),
        ]));
        let mut config = BlueBubblesConfig::new("http://mac.local", "password");
        config.use_private_api = false;
        let channel = BlueBubbles::new(http.factory(), config);
        let mut text = SendMessageRequest::text(target(), "hello");
        text.reply_to = Some("parent-guid".to_owned());
        let chunks = stream::iter([Ok(text_chunk("hel")), Ok(text_chunk("lo"))]);
        let mut streamed = SendMessageRequest::stream(target(), Box::pin(chunks));
        streamed.reply_to = Some("parent-guid".to_owned());
        let attachment = SendMediaRequest {
            target: target(),
            body: BinaryBody::Bytes(vec![1, 2, 3]),
            filename: Some("photo.jpg".to_owned()),
            mime_type: None,
            caption: None,
            reply_to: Some("parent-guid".to_owned()),
        };

        let text = channel
            .send_message(text)
            .await
            .expect("text reply succeeds");
        let streamed = channel
            .send_message(streamed)
            .await
            .expect("streamed reply succeeds");
        let attachment = channel
            .send_media(MediaKind::Image, attachment)
            .await
            .expect("attachment reply succeeds");

        assert_eq!(text.message_id, "text-guid");
        assert_eq!(streamed.message_id, "stream-guid");
        assert_eq!(attachment.message_id, "attachment-guid");
        let requests = http.requests();
        assert_eq!(requests.len(), 3);
        for request in &requests[..2] {
            let json = body_json(request);
            assert_eq!(json["method"], "apple-script");
            assert!(json.get("selectedMessageGuid").is_none());
        }
        assert_eq!(body_json(&requests[1])["message"], "hello");
        let multipart = String::from_utf8_lossy(&requests[2].body);
        assert!(multipart.contains("apple-script"));
        assert!(!multipart.contains("selectedMessageGuid"));
    });
}

#[test]
fn caps_intermediate_stream_edits_and_always_publishes_the_final_text() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            response("stream-guid"),
            response("stream-guid"),
            response("stream-guid"),
        ]));
        let mut config = BlueBubblesConfig::new("http://mac.local", "password");
        config.stream_edit_min_delta_bytes = 1;
        config.stream_max_edits = 2;
        let channel = BlueBubbles::new(http.factory(), config);
        let chunks = stream::iter([
            Ok(text_chunk("a")),
            Ok(text_chunk("b")),
            Ok(text_chunk("c")),
            Ok(text_chunk("d")),
        ]);

        channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await
            .expect("stream succeeds");

        let requests = http.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(body_json(&requests[1])["editedMessage"], "ab");
        assert_eq!(body_json(&requests[2])["editedMessage"], "abcd");
    });
}

#[test]
fn maps_auth_rate_limit_platform_and_invalid_reaction_errors() {
    block_on(async {
        for (status, expected) in [(401, "auth"), (429, "rate"), (500, "platform")] {
            let http = Rc::new(MockHttp::responding([Response {
                status,
                body: format!(
                    r#"{{"status":{status},"message":"failed","error":{{"message":"nope"}}}}"#
                )
                .into_bytes(),
            }]));
            let channel = provider(&http);
            let error = channel
                .send_message(SendMessageRequest::text(target(), "hello"))
                .await
                .expect_err("request should fail");
            assert_eq!(
                matches!(
                    &error,
                    barracuda_imessage_gateway_plugin::ChannelError::Authentication
                ),
                expected == "auth"
            );
            assert_eq!(
                matches!(
                    &error,
                    barracuda_imessage_gateway_plugin::ChannelError::RateLimited
                ),
                expected == "rate"
            );
            assert_eq!(
                matches!(
                    &error,
                    barracuda_imessage_gateway_plugin::ChannelError::Platform { .. }
                ),
                expected == "platform"
            );
        }

        let http = Rc::new(MockHttp::default());
        let channel = provider(&http);
        let error = channel
            .react(ReactRequest::new(target(), "guid", "🔥"))
            .await;
        assert!(matches!(
            error,
            Err(barracuda_imessage_gateway_plugin::ChannelError::InvalidRequest { .. })
        ));
        assert!(http.requests().is_empty());
    });
}

#[test]
fn covers_binary_defaults_validation_and_transport_failures() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response("image-guid")]));
        let channel = provider(&http);
        let request = SendMediaRequest {
            target: target(),
            body: BinaryBody::Bytes(vec![1, 2, 3]),
            filename: Some("image.jpg".to_owned()),
            mime_type: None,
            caption: None,
            reply_to: None,
        };
        channel
            .send_media(MediaKind::Image, request)
            .await
            .expect("image succeeds");
        assert!(!http.requests()[0].body.is_empty());

        let http = Rc::new(MockHttp::default());
        let channel = provider(&http);
        let transport = channel
            .send_message(SendMessageRequest::text(target(), "hello"))
            .await;
        assert!(matches!(
            transport,
            Err(barracuda_imessage_gateway_plugin::ChannelError::Transport { .. })
        ));

        let http = Rc::new(MockHttp::responding([Response {
            status: 200,
            body: b"not-json".to_vec(),
        }]));
        let channel = provider(&http);
        let malformed = channel
            .send_message(SendMessageRequest::text(target(), "hello"))
            .await;
        assert!(matches!(
            malformed,
            Err(barracuda_imessage_gateway_plugin::ChannelError::Platform { .. })
        ));

        let http = Rc::new(MockHttp::default());
        let channel = provider(&http);
        let missing_filename = channel
            .send_media(
                MediaKind::File,
                SendMediaRequest {
                    target: target(),
                    body: BinaryBody::Bytes(vec![1]),
                    filename: None,
                    mime_type: None,
                    caption: None,
                    reply_to: None,
                },
            )
            .await;
        assert!(matches!(
            missing_filename,
            Err(barracuda_imessage_gateway_plugin::ChannelError::InvalidRequest { .. })
        ));
    });
}

#[test]
fn rejects_empty_messages_and_uses_temp_guid_when_server_omits_guid() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([Response {
            status: 200,
            body: br#"{"status":200,"message":"Success"}"#.to_vec(),
        }]));
        let channel = provider(&http);
        let receipt = channel
            .send_message(SendMessageRequest::text(target(), "hello"))
            .await
            .expect("fallback receipt succeeds");
        assert!(receipt.message_id.starts_with("temp-barracuda-"));

        let empty = channel
            .send_message(SendMessageRequest::text(target(), ""))
            .await;
        assert!(matches!(
            empty,
            Err(barracuda_imessage_gateway_plugin::ChannelError::InvalidRequest { .. })
        ));

        let chunks = stream::iter(Vec::<
            Result<TextChunk, barracuda_imessage_gateway_plugin::StreamError>,
        >::new());
        let empty_stream = channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await;
        assert!(matches!(
            empty_stream,
            Err(barracuda_imessage_gateway_plugin::ChannelError::InvalidRequest { .. })
        ));
    });
}

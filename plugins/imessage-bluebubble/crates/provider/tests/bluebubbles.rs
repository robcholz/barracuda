#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use bluebubbles::{BlueBubbles, BlueBubblesConfig};
use futures_lite::{future::block_on, stream};
use gateway::{
    BinaryBody, DeleteMessageRequest, EditMessageRequest, MediaKind, MessageChannel, MessageTarget,
    ReactRequest, SendMediaRequest, SendMessageRequest, SetTypingRequest,
};
use http_client::{Body, Error, HttpClient, HttpFuture, Method, Request, Response};

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

fn response(guid: &str) -> Response {
    Response {
        status: 200,
        body: format!(r#"{{"status":200,"message":"Success","data":{{"guid":"{guid}"}}}}"#)
            .into_bytes(),
    }
}

fn target() -> MessageTarget {
    MessageTarget::new("imessage", "iMessage;-;+15551234567")
}

fn body_json(request: &Request) -> serde_json::Value {
    let Body::Bytes(bytes) = &request.body else {
        panic!("expected JSON body");
    };
    serde_json::from_slice(bytes).expect("valid JSON request")
}

fn provider(http: &Rc<MockHttp>) -> BlueBubbles {
    BlueBubbles::new(
        Rc::clone(http) as Rc<dyn HttpClient>,
        BlueBubblesConfig::new("http://mac.local:1234", "p@ss word&"),
    )
}

#[test]
fn registers_as_imessage_and_sends_replies_with_encoded_auth() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response("message-guid")]));
        let channel = provider(&http);
        let mut request = SendMessageRequest::text(target(), "hello");
        request.reply_to = Some("parent-guid".to_owned());

        let receipt = channel.send_message(request).await.expect("send succeeds");

        assert_eq!(channel.channel(), "imessage");
        assert_eq!(receipt.message_id, "message-guid");
        let requests = http.requests.borrow();
        let request = requests.first().expect("one request");
        assert_eq!(
            request.url,
            "http://mac.local:1234/api/v1/message/text?password=p%40ss%20word%26"
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
        let chunks = stream::iter([Ok("hel".to_owned()), Ok("lo".to_owned())]);

        let receipt = channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await
            .expect("stream succeeds");

        assert_eq!(receipt.message_id, "stream-guid");
        let requests = http.requests.borrow();
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
        let data = stream::iter([Ok(vec![1, 2]), Ok(vec![3, 4])]);
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
        let requests = http.requests.borrow();
        let request = requests.first().expect("one request");
        assert!(request.url.contains("/api/v1/message/attachment?"));
        assert!(request.headers.iter().any(|header| {
            header.name == "Content-Type"
                && header.value.starts_with("multipart/form-data; boundary=")
        }));
        assert!(matches!(&request.body, Body::Stream { .. }));
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

        let requests = http.requests.borrow();
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
        let channel = BlueBubbles::new(Rc::clone(&http) as Rc<dyn HttpClient>, config);
        let chunks = stream::iter([Ok("hel".to_owned()), Ok("lo".to_owned())]);

        channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await
            .expect("fallback send succeeds");
        let edit = channel
            .edit_message(EditMessageRequest::new(target(), "guid", "text"))
            .await;

        assert!(matches!(
            edit,
            Err(gateway::ChannelError::Unsupported { .. })
        ));
        let requests = http.requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(body_json(&requests[0])["message"], "hello");
        assert_eq!(body_json(&requests[0])["method"], "apple-script");
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
        let channel = BlueBubbles::new(Rc::clone(&http) as Rc<dyn HttpClient>, config);
        let chunks = stream::iter([
            Ok("a".to_owned()),
            Ok("b".to_owned()),
            Ok("c".to_owned()),
            Ok("d".to_owned()),
        ]);

        channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await
            .expect("stream succeeds");

        let requests = http.requests.borrow();
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
                matches!(&error, gateway::ChannelError::Authentication),
                expected == "auth"
            );
            assert_eq!(
                matches!(&error, gateway::ChannelError::RateLimited),
                expected == "rate"
            );
            assert_eq!(
                matches!(&error, gateway::ChannelError::Platform { .. }),
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
            Err(gateway::ChannelError::InvalidRequest { .. })
        ));
        assert!(http.requests.borrow().is_empty());
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
        let requests = http.requests.borrow();
        assert!(matches!(&requests[0].body, Body::Bytes(_)));
        drop(requests);

        let http = Rc::new(MockHttp::default());
        let channel = provider(&http);
        let transport = channel
            .send_message(SendMessageRequest::text(target(), "hello"))
            .await;
        assert!(matches!(
            transport,
            Err(gateway::ChannelError::Transport { .. })
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
            Err(gateway::ChannelError::Platform { .. })
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
            Err(gateway::ChannelError::InvalidRequest { .. })
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
            Err(gateway::ChannelError::InvalidRequest { .. })
        ));

        let chunks = stream::iter(Vec::<Result<String, gateway::StreamError>>::new());
        let empty_stream = channel
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await;
        assert!(matches!(
            empty_stream,
            Err(gateway::ChannelError::InvalidRequest { .. })
        ));
    });
}

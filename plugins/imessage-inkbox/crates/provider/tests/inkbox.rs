#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use futures_lite::{future::block_on, stream};
use gateway::{
    BinaryBody, MediaKind, MessageChannel, MessageTarget, ReactRequest, SendMediaRequest,
    SendMessageRequest, SetTypingRequest,
};
use http_client::{Body, Error, HttpClient, HttpFuture, Request, Response};
use inkbox::{Inkbox, InkboxConfig};

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
    MessageTarget::new("imessage", "conversation-uuid")
}

fn provider(http: &Rc<MockHttp>) -> Inkbox {
    Inkbox::new(
        Rc::clone(http) as Rc<dyn HttpClient>,
        InkboxConfig::new("ApiKey_secret", "identity-uuid"),
    )
}

fn body_json(request: &Request) -> serde_json::Value {
    let Body::Bytes(bytes) = &request.body else {
        panic!("expected JSON body")
    };
    serde_json::from_slice(bytes).expect("valid JSON")
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
        let requests = http.requests.borrow();
        let request = requests.first().expect("request");
        assert_eq!(
            request.url,
            "https://inkbox.ai/api/v1/imessage/messages?agent_identity_id=identity-uuid"
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

        let requests = http.requests.borrow();
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
        let requests = http.requests.borrow();
        assert!(requests[0].url.ends_with("/imessage/media"));
        assert!(matches!(
            requests[0].body,
            Body::Bytes(_) | Body::Stream { .. }
        ));
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

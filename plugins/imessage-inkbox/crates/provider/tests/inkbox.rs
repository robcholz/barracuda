#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::{boxed::Box, rc::Rc};

use barracuda_imessage_gateway_plugin::{
    BinaryBody, MediaKind, MessageChannel, MessageTarget, ReactRequest, SendMediaRequest,
    SendMessageRequest, SetTypingRequest, TextChunk,
};
use barracuda_platform_test::{ScriptStep, ScriptedStack};
use futures_lite::{future::block_on, stream};
use http_client::ClientFactory;
use inkbox::{Inkbox, InkboxConfig, InkboxSignup, SignupError, SignupRequest};

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
    method: String,
    url: String,
    headers: Vec<Header>,
    body: Vec<u8>,
}

impl RecordedRequest {
    fn parse(raw: String) -> Self {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let mut lines = head.split("\r\n");
        let mut request_line = lines.next().unwrap_or_default().split_whitespace();
        let method = request_line.next().unwrap_or_default().to_owned();
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

fn signup_client(http: &Rc<MockHttp>) -> InkboxSignup<'static, ScriptedStack, ScriptedStack> {
    InkboxSignup::new(http.factory(), "http://inkbox.test/")
}

fn header<'a>(request: &'a RecordedRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

fn failure(status: u16, body: &str) -> Response {
    Response {
        status,
        body: body.as_bytes().to_vec(),
    }
}

const SIGNUP_RESPONSE: &str = r#"{
    "email_address": "barracuda-a1b2@inkboxmail.com",
    "agent_handle": "barracuda-a1b2",
    "api_key": "ApiKey_once",
    "organization_id": "org-1",
    "claim_status": "agent_unclaimed",
    "human_email": "person@example.com",
    "message": "Verification email sent"
}"#;

#[test]
fn signup_posts_the_unauthenticated_request_and_returns_the_account() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response(SIGNUP_RESPONSE)]));
        let account = signup_client(&http)
            .sign_up(&SignupRequest {
                human_email: "person@example.com",
                note_to_human: "note",
                display_name: Some("Barracuda"),
                harness: Some("barracuda"),
            })
            .await
            .expect("signup succeeds");

        assert_eq!(account.api_key, "ApiKey_once");
        assert_eq!(account.agent_handle, "barracuda-a1b2");
        assert_eq!(account.email_address, "barracuda-a1b2@inkboxmail.com");
        assert_eq!(account.claim_status, "agent_unclaimed");
        let requests = http.requests();
        let request = requests.first().expect("request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/api/v1/agent-signup");
        assert_eq!(header(request, "X-API-Key"), None);
        assert_eq!(header(request, "Content-Type"), Some("application/json"));
        assert_eq!(
            body_json(request),
            serde_json::json!({
                "human_email": "person@example.com",
                "note_to_human": "note",
                "display_name": "Barracuda",
                "harness": "barracuda",
            })
        );
    });
}

#[test]
fn resolves_the_agent_handle_to_its_identity_id() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            response(
                r#"{"id":"eeee5555-0000-0000-0000-000000000001","agent_handle":"barracuda-a1b2"}"#,
            ),
            response(r#"{"id":"encoded-id"}"#),
        ]));
        let client = signup_client(&http);
        let identity = client
            .resolve_identity("ApiKey_once", "barracuda-a1b2")
            .await
            .expect("identity resolves");
        client
            .resolve_identity("ApiKey_once", "odd handle/?")
            .await
            .expect("encoded identity resolves");

        assert_eq!(identity, "eeee5555-0000-0000-0000-000000000001");
        let requests = http.requests();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].url, "/api/v1/identities/barracuda-a1b2");
        assert_eq!(header(&requests[0], "X-API-Key"), Some("ApiKey_once"));
        assert_eq!(header(&requests[0], "Content-Length"), Some("0"));
        assert_eq!(requests[1].url, "/api/v1/identities/odd%20handle%2F%3F");
    });
}

#[test]
fn identity_without_an_id_is_unavailable() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response(r#"{"agent_handle":"x"}"#)]));
        let error = signup_client(&http)
            .resolve_identity("ApiKey_once", "x")
            .await
            .expect_err("missing id fails");
        assert!(matches!(error, SignupError::Unavailable { .. }));
    });
}

#[test]
fn passes_upstream_rejections_through() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            failure(
                422,
                r#"{"detail":[{"loc":["body","human_email"],"msg":"value is not a valid email address","type":"value_error"}]}"#,
            ),
            failure(
                429,
                r#"{"detail":{"code":"signup_rate_limited","message":"Too many signups."}}"#,
            ),
        ]));
        let client = signup_client(&http);
        let request = SignupRequest {
            human_email: "not-an-email",
            note_to_human: "note",
            display_name: None,
            harness: None,
        };
        let validation = client.sign_up(&request).await.err().expect("422 fails");
        let limited = client.sign_up(&request).await.err().expect("429 fails");

        assert!(matches!(
            validation,
            SignupError::Rejected { status: 422, code: None, message: Some(ref message) }
                if message == "value is not a valid email address"
        ));
        assert!(matches!(
            limited,
            SignupError::Rejected { status: 429, code: Some(ref code), message: Some(ref message) }
                if code == "signup_rate_limited" && message == "Too many signups."
        ));
        assert_eq!(
            body_json(&http.requests()[0]),
            serde_json::json!({"human_email": "not-an-email", "note_to_human": "note"})
        );
    });
}

#[test]
fn verifies_the_code_with_the_api_key() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            response(
                r#"{"claim_status":"agent_claimed","organization_id":"org-2","message":"ok"}"#,
            ),
            failure(422, r#"{"detail":"Invalid verification code"}"#),
        ]));
        let client = signup_client(&http);
        let claim_status = client
            .verify("ApiKey_once", "483921")
            .await
            .expect("verification succeeds");
        let wrong = client
            .verify("ApiKey_once", "000000")
            .await
            .expect_err("wrong code fails");

        assert_eq!(claim_status, "agent_claimed");
        assert!(matches!(
            wrong,
            SignupError::Rejected { status: 422, message: Some(ref message), .. }
                if message == "Invalid verification code"
        ));
        let requests = http.requests();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].url, "/api/v1/agent-signup/verify");
        assert_eq!(header(&requests[0], "X-API-Key"), Some("ApiKey_once"));
        assert_eq!(
            body_json(&requests[0]),
            serde_json::json!({"verification_code": "483921"})
        );
    });
}

#[test]
fn resends_verification_without_a_body() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([response(
            r#"{"claim_status":"agent_unclaimed","organization_id":"org-1","message":"sent"}"#,
        )]));
        signup_client(&http)
            .resend_verification("ApiKey_once")
            .await
            .expect("resend succeeds");

        let requests = http.requests();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].url, "/api/v1/agent-signup/resend-verification");
        assert_eq!(header(&requests[0], "X-API-Key"), Some("ApiKey_once"));
        assert_eq!(header(&requests[0], "Content-Length"), Some("0"));
        assert_eq!(header(&requests[0], "Content-Type"), None);
        assert!(requests[0].body.is_empty());
    });
}

#[test]
fn server_errors_and_non_json_replies_are_unavailable() {
    block_on(async {
        let http = Rc::new(MockHttp::responding([
            failure(503, r#"{"detail":"maintenance"}"#),
            response("<html>gateway</html>"),
        ]));
        let client = signup_client(&http);
        let server = client
            .resend_verification("ApiKey_once")
            .await
            .expect_err("503 fails");
        let html = client
            .resend_verification("ApiKey_once")
            .await
            .expect_err("HTML fails");

        assert!(matches!(
            server,
            SignupError::Unavailable { message: Some(ref message), .. } if message == "maintenance"
        ));
        assert!(matches!(html, SignupError::Unavailable { .. }));
    });
}

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! The Inkbox provider and signup client against a scripted Inkbox API,
//! sending through a real Gateway's HTTP on a loopback network.

use std::{boxed::Box, cell::RefCell, rc::Rc};

use barracuda_http_plugin::HttpPlugin;
use barracuda_imessage_gateway_plugin::{
    BinaryBody, GatewayHttp, IMessageGateway, IMessageGatewayPlugin, MediaKind, MessageChannel,
    MessageTarget, ReactRequest, SendMediaRequest, SendMessageRequest, SetTypingRequest, TextChunk,
};
use barracuda_platform_test::{
    install_global_memory_vfs, loopback_network, memory_partition, ScriptStep, ScriptedServer,
};
use barracuda_plugin::api::{BoardInfo, Hardware, PlatformInfo, PluginContext, TargetIdentity};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_webserver_plugin::WebServerPlugin;
use barracuda_workflow_plugin::WorkflowPlugin;
use embassy_futures::select::{select3, Either3};
use futures_lite::{future::block_on, stream};
use inkbox::{Inkbox, InkboxConfig, InkboxSignup, SignupError, SignupRequest};

/// Port of the scripted Inkbox API on the loopback network.
const PORT: u16 = 9000;
const API_BASE: &str = "http://10.0.0.1:9000";

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

/// Captures the Gateway capability during registration.
struct Probe(Rc<RefCell<Option<Rc<IMessageGateway>>>>);

impl PluginDeclaration for Probe {
    const ID: &'static str = "inkbox-provider-probe";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway"];
}

impl Plugin for Probe {
    fn register<Storage: PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        self.0.replace(Some(
            context.require::<IMessageGateway>("imessage-gateway")?,
        ));
        Ok(())
    }
}

/// Runs `test` with the HTTP of a real Gateway (beside Workflow, HTTP, and
/// WebServer) on a loopback network, and an Inkbox API there answering
/// `responses` in order.
fn bench(
    responses: impl IntoIterator<Item = Response>,
    test: impl AsyncFnOnce(GatewayHttp, &ScriptedServer),
) {
    let network = loopback_network();
    let stack = network.stack();
    let server = ScriptedServer::new(responses.into_iter().map(|response| {
        ScriptStep::response(
            response.status,
            "application/json",
            &response.body,
            usize::MAX,
        )
    }));
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let identity = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    let mut context = PluginContext::new(identity, stack);
    let gateway = Rc::new(RefCell::new(None));
    manager
        .register(WorkflowPlugin::new(&mut context))
        .expect("register Workflow");
    manager
        .register(HttpPlugin::new(&mut context))
        .expect("register HTTP");
    manager
        .register(WebServerPlugin::new(&mut context))
        .expect("register WebServer");
    manager
        .register(IMessageGatewayPlugin::new(&mut context))
        .expect("register Gateway");
    manager
        .register(Probe(Rc::clone(&gateway)))
        .expect("register probe");
    let http = gateway.take().expect("Gateway capability").http();
    let outcome = block_on(select3(
        network.run(),
        server.run::<4>(stack, PORT),
        test(http, &server),
    ));
    assert!(matches!(outcome, Either3::Third(())), "the bench stopped");
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

fn requests(server: &ScriptedServer) -> Vec<RecordedRequest> {
    server
        .requests()
        .into_iter()
        .map(RecordedRequest::parse)
        .collect()
}

fn response(body: &str) -> Response {
    Response {
        status: 200,
        body: body.as_bytes().to_vec(),
    }
}

fn target() -> MessageTarget {
    MessageTarget::new("inkbox", "conversation-uuid")
}

fn text_chunk(text: &str) -> TextChunk {
    TextChunk::inline(text).expect("test text chunk fits inline storage")
}

fn provider(http: GatewayHttp) -> Inkbox {
    let mut config = InkboxConfig::new("ApiKey_secret", "identity-uuid");
    config.api_base = API_BASE.to_owned();
    Inkbox::new(http, config)
}

fn body_json(request: &RecordedRequest) -> serde_json::Value {
    serde_json::from_slice(&request.body).expect("valid JSON")
}

#[test]
fn registers_as_inkbox_and_sends_text_for_the_configured_identity() {
    bench(
        [response(r#"{"message":{"id":"message-uuid"}}"#)],
        async |http, server| {
            let channel = provider(http);
            let receipt = channel
                .send_message(SendMessageRequest::text(target(), "hello"))
                .await
                .expect("send succeeds");

            assert_eq!(channel.channel(), "inkbox");
            assert_eq!(receipt.message_id, "message-uuid");
            let requests = requests(server);
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
        },
    );
}

#[test]
fn buffers_streams_and_maps_tapbacks_and_typing() {
    bench(
        [
            response(r#"{"message":{"id":"stream-id"}}"#),
            response(r#"{"id":"reaction-id"}"#),
            response(r#"{}"#),
        ],
        async |http, server| {
            let channel = provider(http);
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

            let requests = requests(server);
            assert_eq!(requests.len(), 3);
            assert_eq!(body_json(&requests[0])["text"], "hello");
            assert_eq!(body_json(&requests[1])["reaction"], "like");
            assert_eq!(
                body_json(&requests[2])["conversation_id"],
                "conversation-uuid"
            );
        },
    );
}

#[test]
fn uploads_media_then_sends_the_returned_url() {
    bench(
        [
            response(r#"{"media_url":"https://media.example/photo.jpg"}"#),
            response(r#"{"message":{"id":"media-id"}}"#),
        ],
        async |http, server| {
            let channel = provider(http);
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
            let requests = requests(server);
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
        },
    );
}

#[test]
fn ignores_reply_to_for_text_and_media() {
    bench(
        [
            response(r#"{"message":{"id":"reply-id"}}"#),
            response(r#"{"media_url":"https://media.example/photo.jpg"}"#),
            response(r#"{"message":{"id":"media-id"}}"#),
        ],
        async |http, server| {
            let channel = provider(http);
            let mut text = SendMessageRequest::text(target(), "hello");
            text.reply_to = Some("inbound-id".to_owned());
            let media = SendMediaRequest {
                target: target(),
                body: BinaryBody::Bytes(vec![1, 2, 3]),
                filename: None,
                mime_type: None,
                caption: None,
                reply_to: Some("inbound-id".to_owned()),
            };

            let receipt = channel.send_message(text).await.expect("reply succeeds");
            let media_receipt = channel
                .send_media(MediaKind::Image, media)
                .await
                .expect("media reply succeeds");

            assert_eq!(receipt.message_id, "reply-id");
            assert_eq!(media_receipt.message_id, "media-id");
            let requests = requests(server);
            assert_eq!(requests.len(), 3);
            assert_eq!(
                body_json(&requests[0]),
                serde_json::json!({"conversation_id": "conversation-uuid", "text": "hello"})
            );
            assert!(body_json(&requests[2]).get("reply_to").is_none());
        },
    );
}

#[test]
fn maps_authentication_and_rate_limits() {
    let refused = |status| Response {
        status,
        body: br#"{"detail":"failed"}"#.to_vec(),
    };
    bench([refused(401), refused(429)], async |http, _server| {
        for authentication in [true, false] {
            let error = provider(http.clone())
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

fn signup_client(http: GatewayHttp) -> InkboxSignup {
    InkboxSignup::new(http, format!("{API_BASE}/"))
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
    bench([response(SIGNUP_RESPONSE)], async |http, server| {
        let account = signup_client(http)
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
        let requests = requests(server);
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
    bench(
        [
            response(
                r#"{"id":"eeee5555-0000-0000-0000-000000000001","agent_handle":"barracuda-a1b2"}"#,
            ),
            response(r#"{"id":"encoded-id"}"#),
        ],
        async |http, server| {
            let client = signup_client(http);
            let identity = client
                .resolve_identity("ApiKey_once", "barracuda-a1b2")
                .await
                .expect("identity resolves");
            client
                .resolve_identity("ApiKey_once", "odd handle/?")
                .await
                .expect("encoded identity resolves");

            assert_eq!(identity, "eeee5555-0000-0000-0000-000000000001");
            let requests = requests(server);
            assert_eq!(requests[0].method, "GET");
            assert_eq!(requests[0].url, "/api/v1/identities/barracuda-a1b2");
            assert_eq!(header(&requests[0], "X-API-Key"), Some("ApiKey_once"));
            assert_eq!(header(&requests[0], "Content-Length"), Some("0"));
            assert_eq!(requests[1].url, "/api/v1/identities/odd%20handle%2F%3F");
        },
    );
}

#[test]
fn identity_without_an_id_is_unavailable() {
    bench(
        [response(r#"{"agent_handle":"x"}"#)],
        async |http, _server| {
            let error = signup_client(http)
                .resolve_identity("ApiKey_once", "x")
                .await
                .expect_err("missing id fails");
            assert!(matches!(error, SignupError::Unavailable { .. }));
        },
    );
}

#[test]
fn passes_upstream_rejections_through() {
    bench(
        [
            failure(
                422,
                r#"{"detail":[{"loc":["body","human_email"],"msg":"value is not a valid email address","type":"value_error"}]}"#,
            ),
            failure(
                429,
                r#"{"detail":{"code":"signup_rate_limited","message":"Too many signups."}}"#,
            ),
        ],
        async |http, server| {
            let client = signup_client(http);
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
                body_json(&requests(server)[0]),
                serde_json::json!({"human_email": "not-an-email", "note_to_human": "note"})
            );
        },
    );
}

#[test]
fn verifies_the_code_with_the_api_key() {
    bench(
        [
            response(
                r#"{"claim_status":"agent_claimed","organization_id":"org-2","message":"ok"}"#,
            ),
            failure(422, r#"{"detail":"Invalid verification code"}"#),
        ],
        async |http, server| {
            let client = signup_client(http);
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
            let requests = requests(server);
            assert_eq!(requests[0].method, "POST");
            assert_eq!(requests[0].url, "/api/v1/agent-signup/verify");
            assert_eq!(header(&requests[0], "X-API-Key"), Some("ApiKey_once"));
            assert_eq!(
                body_json(&requests[0]),
                serde_json::json!({"verification_code": "483921"})
            );
        },
    );
}

#[test]
fn resends_verification_without_a_body() {
    bench(
        [response(
            r#"{"claim_status":"agent_unclaimed","organization_id":"org-1","message":"sent"}"#,
        )],
        async |http, server| {
            signup_client(http)
                .resend_verification("ApiKey_once")
                .await
                .expect("resend succeeds");

            let requests = requests(server);
            assert_eq!(requests[0].method, "POST");
            assert_eq!(requests[0].url, "/api/v1/agent-signup/resend-verification");
            assert_eq!(header(&requests[0], "X-API-Key"), Some("ApiKey_once"));
            assert_eq!(header(&requests[0], "Content-Length"), Some("0"));
            assert_eq!(header(&requests[0], "Content-Type"), None);
            assert!(requests[0].body.is_empty());
        },
    );
}

#[test]
fn server_errors_and_non_json_replies_are_unavailable() {
    bench(
        [
            failure(503, r#"{"detail":"maintenance"}"#),
            response("<html>gateway</html>"),
        ],
        async |http, _server| {
            let client = signup_client(http);
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
        },
    );
}

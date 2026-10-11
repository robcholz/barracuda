#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! The Telegram provider against a scripted Bot API, sending through a real
//! Gateway's HTTP on a loopback network.

use std::{boxed::Box, cell::RefCell, rc::Rc};

use barracuda_http_plugin::HttpPlugin;
use barracuda_imessage_gateway_plugin::{
    BinaryBody, BinaryChunk, DeleteMessageRequest, EditMessageRequest, GatewayHttp,
    IMessageGateway, IMessageGatewayPlugin, MediaKind, MessageChannel, MessageTarget, ReactRequest,
    SendMediaRequest, SendMessageRequest, SetTypingRequest, TextChunk,
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
use telegram::{Telegram, TelegramConfig};

/// Port of the scripted Bot API on the loopback network.
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
    const ID: &'static str = "telegram-provider-probe";
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
/// WebServer) on a loopback network, and a Bot API there answering
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
    MessageTarget::new("telegram", "42")
}

fn config(token: &str) -> TelegramConfig {
    let mut config = TelegramConfig::new(token);
    config.api_base = API_BASE.to_owned();
    config
}

fn body_json(request: &RecordedRequest) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null)
}

#[test]
fn registers_as_the_telegram_channel_and_sends_text() {
    bench(
        [response(r#"{"ok":true,"result":{"message_id":7}}"#)],
        async |http, server| {
            let channel = Telegram::new(http, config("bot-token"));

            let receipt = channel
                .send_message(SendMessageRequest::text(target(), "hello"))
                .await;

            assert_eq!(channel.channel(), "telegram");
            assert!(matches!(receipt, Ok(receipt) if receipt.message_id == "7"));
            let requests = requests(server);
            let Some(request) = requests.first() else {
                panic!("missing request");
            };
            assert_eq!(request.url, "/botbot-token/sendMessage");
            assert_eq!(body_json(request)["chat_id"], 42);
            assert_eq!(body_json(request)["text"], "hello");
        },
    );
}

#[test]
fn maps_text_chunks_to_drafts_and_finishes_with_a_normal_message() {
    bench(
        [
            response(r#"{"ok":true,"result":true}"#),
            response(r#"{"ok":true,"result":true}"#),
            response(r#"{"ok":true,"result":{"message_id":9}}"#),
        ],
        async |http, server| {
            let mut settings = config("token");
            settings.draft_min_delta_bytes = 1;
            let channel = Telegram::new(http, settings);
            let chunks = stream::iter([
                Ok(TextChunk::inline("hel").expect("chunk fits inline storage")),
                Ok(TextChunk::inline("lo").expect("chunk fits inline storage")),
            ]);

            let result = channel
                .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
                .await;

            assert!(matches!(result, Ok(receipt) if receipt.message_id == "9"));
            let requests = requests(server);
            assert_eq!(requests.len(), 3);
            assert!(requests
                .first()
                .is_some_and(|request| request.url.ends_with("/sendMessageDraft")));
            assert_eq!(
                requests.first().map(body_json).unwrap_or_default()["text"],
                "hel"
            );
            assert_eq!(
                requests.get(1).map(body_json).unwrap_or_default()["text"],
                "hello"
            );
            assert!(requests
                .get(2)
                .is_some_and(|request| request.url.ends_with("/sendMessage")));
        },
    );
}

#[test]
fn sends_images_as_multipart_without_buffering_stream_inputs() {
    bench(
        [response(r#"{"ok":true,"result":{"message_id":11}}"#)],
        async |http, server| {
            let channel = Telegram::new(http, config("token"));
            let data = stream::iter([
                Ok(BinaryChunk::from(vec![1, 2])),
                Ok(BinaryChunk::from(vec![3, 4])),
            ]);
            let request = SendMediaRequest {
                target: target(),
                body: BinaryBody::Stream(Box::pin(data)),
                size: None,
                filename: Some("photo.jpg".to_owned()),
                mime_type: Some("image/jpeg".to_owned()),
                caption: Some("caption".to_owned()),
                reply_to: None,
            };

            let result = channel.send_media(MediaKind::Image, request).await;

            assert!(matches!(result, Ok(receipt) if receipt.message_id == "11"));
            let requests = requests(server);
            let Some(request) = requests.first() else {
                panic!("missing request");
            };
            assert!(request.url.ends_with("/sendPhoto"));
            assert!(request.headers.iter().any(|header| {
                header.name == "Content-Type"
                    && header.value.starts_with("multipart/form-data; boundary=")
            }));
            assert!(request
                .headers
                .iter()
                .any(|header| { header.name == "Transfer-Encoding" && header.value == "chunked" }));
        },
    );
}

#[test]
fn maps_message_mutations_reactions_and_typing() {
    bench(
        [
            response(r#"{"ok":true,"result":{"message_id":7}}"#),
            response(r#"{"ok":true,"result":true}"#),
            response(r#"{"ok":true,"result":true}"#),
            response(r#"{"ok":true,"result":true}"#),
        ],
        async |http, server| {
            let channel = Telegram::new(http, config("token"));

            channel
                .edit_message(EditMessageRequest::new(target(), "7", "edited"))
                .await
                .expect("edit succeeds");
            channel
                .delete_message(DeleteMessageRequest::new(target(), "7"))
                .await
                .expect("delete succeeds");
            channel
                .react(ReactRequest::new(target(), "7", "👍"))
                .await
                .expect("reaction succeeds");
            channel
                .set_typing(SetTypingRequest::new(target(), true))
                .await
                .expect("typing succeeds");
            channel
                .set_typing(SetTypingRequest::new(target(), false))
                .await
                .expect("stopping typing is local");

            let requests = requests(server);
            let methods: Vec<&str> = requests
                .iter()
                .filter_map(|request| request.url.rsplit('/').next())
                .collect();
            assert_eq!(
                methods,
                [
                    "editMessageText",
                    "deleteMessage",
                    "setMessageReaction",
                    "sendChatAction"
                ]
            );
            assert_eq!(body_json(&requests[0])["message_id"], 7);
            assert_eq!(body_json(&requests[0])["text"], "edited");
            assert_eq!(body_json(&requests[2])["reaction"][0]["emoji"], "👍");
            assert_eq!(body_json(&requests[3])["action"], "typing");
        },
    );
}

#[test]
fn maps_telegram_authentication_and_rate_limit_errors() {
    let refused = |status| Response {
        status,
        body: br#"{"ok":false}"#.to_vec(),
    };
    bench([refused(401), refused(429)], async |http, _server| {
        for expected_auth in [true, false] {
            let channel = Telegram::new(http.clone(), config("token"));
            let error = channel
                .send_message(SendMessageRequest::text(target(), "hello"))
                .await
                .expect_err("request should fail");
            assert_eq!(
                matches!(
                    &error,
                    barracuda_imessage_gateway_plugin::ChannelError::Authentication
                ),
                expected_auth
            );
            assert_eq!(
                matches!(
                    &error,
                    barracuda_imessage_gateway_plugin::ChannelError::RateLimited
                ),
                !expected_auth
            );
        }
    });
}

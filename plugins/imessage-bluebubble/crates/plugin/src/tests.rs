//! The channel against a real Gateway and Workflow, a scripted BlueBubbles
//! server, and real Plugin storage.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;

use barracuda_imessage_gateway_channel::{
    receive_runtime, sync_receive, ChannelControl, ChannelMode, ModeEndpoint, OwnersEndpoint,
    ReceiveState, ReceiveTiming, MODE_STORAGE_KEY, PAIRED_REPLY,
};
use barracuda_imessage_gateway_plugin::{
    ChannelError, ChannelFuture, GatewayInboundMessage, GatewayIngressError, IMessageGateway,
    IMessageGatewayPlugin, MessageChannel, Operation, SendMessageRequest, SendReceipt,
};
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_platform_test::{
    install_global_memory_vfs, memory_partition, ScriptStep, ScriptedStack,
};
use barracuda_plugin::api::SharedEntropy;
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_webserver_plugin::{HttpEndpoint, HttpMethod, HttpRequest};
use barracuda_workflow_plugin::WorkflowPlugin;
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Timer};
use futures_lite::future::block_on;
use http_client::ClientFactory;

use super::*;
use crate::channel::{SinkFuture, CONFIGURATION_STORAGE_KEY};
use crate::state::{CURSOR_STORAGE_KEY, HOOK_STORAGE_KEY};
use crate::webhook::{Delivery, WEBHOOK_BODY_MAX};

const TIMING: ReceiveTiming = ReceiveTiming {
    initial_backoff: Duration::from_millis(20),
    max_backoff: Duration::from_millis(40),
    slot_retry: Duration::from_millis(20),
};
const OWNER: &str = "+15550001111";
const STRANGER: &str = "+15559990000";
const ADDRESS: &str = "192.168.1.50";

type Channel<S> = BlueBubblesChannel<S, ScriptedStack, ScriptedStack>;

/// Fills every request with the same bytes, so secrets and codes repeat.
#[derive(Clone)]
struct Pattern;

impl Entropy for Pattern {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::try_from(index % 256)
                .unwrap_or(0)
                .wrapping_mul(37)
                .wrapping_add(11);
        }
        Ok(())
    }
}

/// Records published messages instead of starting Workflow executions.
#[derive(Default)]
struct Recorder(RefCell<Vec<GatewayInboundMessage>>);

impl Recorder {
    fn published(&self) -> Vec<GatewayInboundMessage> {
        self.0.borrow().clone()
    }
}

impl InboundSink for Recorder {
    fn ready(&self) -> SinkFuture<'_, ()> {
        Box::pin(async {})
    }

    fn publish(
        &self,
        message: GatewayInboundMessage,
    ) -> SinkFuture<'_, Result<(), GatewayIngressError>> {
        self.0.borrow_mut().push(message);
        Box::pin(async { Ok(()) })
    }
}

/// Holds the `bluebubbles` channel name, to see whether the channel is
/// registered.
struct Occupant;

impl MessageChannel for Occupant {
    fn channel(&self) -> &str {
        bluebubbles::CHANNEL
    }

    fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::SendMessage)) })
    }
}

fn is_registered(gateway: &IMessageGateway) -> bool {
    let occupant: Rc<dyn MessageChannel> = Rc::new(Occupant);
    gateway.register(occupant).is_err()
}

trait Scenario {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S);
}

struct Probe<Sc>(Option<Sc>);

impl<Sc> PluginDeclaration for Probe<Sc> {
    const ID: &'static str = "bluebubbles-probe";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway"];
}

impl<Sc: Scenario + 'static> Plugin for Probe<Sc> {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        if let Some(scenario) = self.0.take() {
            scenario.run(gateway, context.storage().clone());
        }
        Ok(())
    }
}

fn plugin_context() -> PluginContext {
    let stack = barracuda_platform_test::never_embassy_stack();
    let info = barracuda_plugin::api::TargetIdentity::new(
        barracuda_plugin::api::PlatformInfo::new("test", "test", "test-arch", "hosted"),
        barracuda_plugin::api::BoardInfo::new(
            "test-board",
            barracuda_plugin::api::Hardware::new("test-chip"),
        ),
    );
    PluginContext::new(info, stack, ClientFactory::plaintext(stack))
}

/// Runs `scenario` inside a Plugin registered after Workflow and the Gateway.
fn with_gateway(scenario: impl Scenario + 'static) {
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let mut context = plugin_context();
    manager
        .register(WorkflowPlugin::new(&mut context))
        .expect("register Workflow");
    manager
        .register(IMessageGatewayPlugin::new(&mut context))
        .expect("register Gateway");
    manager
        .register(Probe(Some(scenario)))
        .expect("run the scenario");
}

/// A scripted BlueBubbles server answering `steps` in order.
fn server(steps: Vec<ScriptStep>) -> &'static ScriptedStack {
    Box::leak(Box::new(ScriptedStack::new(steps)))
}

fn ok(data: &str) -> ScriptStep {
    ScriptStep::json(
        200,
        &format!(r#"{{"status":200,"message":"Success","data":{data}}}"#),
    )
}

fn page(messages: &[String], total: usize) -> ScriptStep {
    ScriptStep::json(
        200,
        &format!(
            r#"{{"status":200,"message":"Success","data":[{}],"metadata":{{"offset":0,"limit":10,"total":{total},"count":{}}}}}"#,
            messages.join(","),
            messages.len()
        ),
    )
}

fn message(guid: &str, sender: &str, text: Option<&str>, from_me: bool, date: u64) -> String {
    let text = text.map_or_else(
        || "null".to_string(),
        |text| serde_json::to_string(text).expect("text JSON"),
    );
    format!(
        r#"{{"originalROWID":7,"guid":"{guid}","text":{text},"attributedBody":null,"handle":{{"originalROWID":3,"address":"{sender}","service":"iMessage","country":"us"}},"handleId":3,"isFromMe":{from_me},"dateCreated":{date},"attachments":[],"itemType":0,"associatedMessageGuid":null,"associatedMessageType":null,"chats":[{{"originalROWID":2,"guid":"{}","style":45,"chatIdentifier":"{sender}","isArchived":false,"displayName":""}},{{"guid":"ignored"}}]}}"#,
        chat(sender)
    )
}

fn chat(sender: &str) -> String {
    format!("iMessage;-;{sender}")
}

fn event(kind: &str, data: &str) -> Vec<u8> {
    format!(r#"{{"type":"{kind}","data":{data}}}"#).into_bytes()
}

/// The first session of a channel with no cursor: the newest message is
/// `old`, no webhook exists yet, and nothing came after `old`.
fn fresh_start() -> Vec<ScriptStep> {
    let old = message("old", OWNER, Some("before"), false, 1_000);
    vec![
        page(core::slice::from_ref(&old), 9),
        ok("[]"),
        ok(r#"{"id":1,"url":"x","events":["new-message"]}"#),
        page(&[old], 1),
    ]
}

/// One recorded request: method, path with query, and body.
fn requests(server: &ScriptedStack) -> Vec<(String, String, String)> {
    server
        .requests()
        .into_iter()
        .map(|raw| {
            let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
            let mut line = head.lines().next().unwrap_or_default().split_whitespace();
            let method = line.next().unwrap_or_default().to_string();
            let path = line.next().unwrap_or_default().to_string();
            (method, path, body.to_string())
        })
        .collect()
}

async fn store_configuration<S: PluginStorage>(storage: &S, mode: Option<&str>, owners: &[&str]) {
    storage
        .put(
            CONFIGURATION_STORAGE_KEY,
            br#"{"server_url":"http://blue.test","password":"pw","use_private_api":false}"#
                .as_slice(),
        )
        .await
        .expect("store configuration");
    if let Some(mode) = mode {
        storage
            .put(MODE_STORAGE_KEY, format!("\"{mode}\"").as_bytes())
            .await
            .expect("store mode");
    }
    let owners = owners
        .iter()
        .map(|id| format!(r#"{{"id":"{id}","label":null}}"#))
        .collect::<Vec<_>>()
        .join(",");
    storage
        .put("owners", format!("[{owners}]").as_bytes())
        .await
        .expect("store owners");
}

async fn load<S: PluginStorage>(
    gateway: &Rc<IMessageGateway>,
    storage: &S,
    server: &'static ScriptedStack,
    recorder: &Rc<Recorder>,
    address: Option<&'static str>,
) -> Rc<Channel<S>> {
    let inbound: Rc<dyn InboundSink> = recorder.clone();
    Rc::new(
        BlueBubblesChannel::load(ChannelSetup {
            gateway: Rc::clone(gateway),
            inbound,
            http_clients: ClientFactory::from_network(server, server),
            storage: storage.clone(),
            entropy: SharedEntropy::new(Pattern),
            address: Box::new(move || address.map(String::from)),
            port: WEB_SERVER_PORT,
        })
        .await
        .expect("load the channel"),
    )
}

/// Runs the channel's receive runtime while `test` runs.
async fn receiving<S: PluginStorage, F: Future>(channel: &Rc<Channel<S>>, test: F) -> F::Output {
    let runtime = receive_runtime(
        channel.receive_control(),
        Rc::new(Receiver(Rc::clone(channel))),
        TIMING,
    );
    sync_receive(&**channel).ok();
    match select(runtime, test).await {
        Either::First(()) => panic!("the receive runtime ended"),
        Either::Second(output) => output,
    }
}

/// Waits up to two seconds for `condition`.
async fn until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        Timer::after(Duration::from_millis(2)).await;
    }
}

/// The secret [`Pattern`] mints.
fn expected_secret() -> String {
    let mut bytes = [0_u8; 16];
    Pattern.fill(&mut bytes).expect("pattern");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hook_path<S: PluginStorage>(channel: &Channel<S>) -> String {
    format!(
        "{HOOK_PATH}/{}",
        channel.book.hook().expect("webhook record").secret
    )
}

async fn post_hook<S: PluginStorage>(channel: &Rc<Channel<S>>, path: &str, body: Vec<u8>) -> u16 {
    WebhookEndpoint(Rc::clone(channel))
        .handle(HttpRequest::with_path(HttpMethod::Post, path.into(), body))
        .await
        .status()
}

async fn call(endpoint: &dyn HttpEndpoint, method: HttpMethod, body: &str) -> (u16, String) {
    let response = endpoint
        .handle(HttpRequest::new(method, body.as_bytes().to_vec()))
        .await;
    let status = response.status();
    let body = String::from_utf8(response.body().expect("buffered").to_vec()).expect("UTF-8");
    (status, body)
}

struct OwnerMessageIsPublished;

impl Scenario for OwnerMessageIsPublished {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S) {
        block_on(async {
            store_configuration(&storage, Some("send_receive"), &[OWNER]).await;
            let server = server(fresh_start());
            let recorder = Rc::new(Recorder::default());
            let channel = load(&gateway, &storage, server, &recorder, Some(ADDRESS)).await;
            assert!(is_registered(&gateway));
            receiving(&channel, async {
                until("the catch-up", || server.remaining() == 0).await;
                assert_eq!(channel.receive().state(), ReceiveState::Receiving);
                let path = hook_path(&channel);
                let body = event(
                    "new-message",
                    &message("m1", OWNER, Some("hello"), false, 2_000),
                );
                assert_eq!(post_hook(&channel, &path, body).await, 204);
                until("the publish", || !recorder.published().is_empty()).await;
            })
            .await;

            let published = recorder.published();
            assert_eq!(published.len(), 1, "`old` was the cursor start");
            assert_eq!(published[0].route.channel, "bluebubbles");
            assert_eq!(published[0].route.conversation_id, chat(OWNER));
            assert_eq!(published[0].route.thread_id, None);
            assert_eq!(published[0].message_id, "m1");
            assert_eq!(published[0].text, "hello");

            let secret = channel.book.hook().expect("record").secret;
            assert_eq!(secret.len(), 32);
            let url = format!("http://{ADDRESS}:8787/api/gateway/bluebubbles/hook/{secret}");
            let requests = requests(server);
            assert_eq!(requests.len(), 4);
            assert_eq!(requests[0].0, "POST");
            assert_eq!(requests[0].1, "/api/v1/message/query?password=pw");
            assert!(requests[0].2.contains(r#""sort":"DESC""#));
            assert!(requests[0].2.contains(r#""limit":1"#));
            assert_eq!(
                (requests[1].0.as_str(), requests[1].1.as_str()),
                ("GET", "/api/v1/webhook?password=pw")
            );
            assert_eq!(
                (requests[2].0.as_str(), requests[2].1.as_str()),
                ("POST", "/api/v1/webhook?password=pw")
            );
            assert_eq!(
                requests[2].2,
                format!(r#"{{"events":["new-message"],"url":"{url}"}}"#)
            );
            assert!(requests[3].2.contains(r#""after":1000"#));
            assert!(requests[3].2.contains(r#""sort":"ASC""#));
            assert!(requests[3].2.contains(r#""with":["chat"]"#));
            assert_eq!(channel.book.hook().and_then(|hook| hook.url), Some(url));
        });
    }
}

#[test]
fn an_owner_message_is_published_on_the_chat_route() {
    with_gateway(OwnerMessageIsPublished);
}

struct PairingAndStrangers;

impl Scenario for PairingAndStrangers {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S) {
        block_on(async {
            store_configuration(&storage, Some("send_receive"), &[]).await;
            let mut steps = fresh_start();
            steps.push(ok(r#"{"guid":"sent-1"}"#));
            let server = server(steps);
            let recorder = Rc::new(Recorder::default());
            let channel = load(&gateway, &storage, server, &recorder, Some(ADDRESS)).await;
            receiving(&channel, async {
                until("the catch-up", || server.remaining() == 1).await;
                let path = hook_path(&channel);
                let stranger = event(
                    "new-message",
                    &message("s1", STRANGER, Some("let me in"), false, 2_000),
                );
                assert_eq!(post_hook(&channel, &path, stranger).await, 204);
                until("the stranger to be ignored", || {
                    channel.owners().expect("owners").ignored() == 1
                })
                .await;

                let code = channel
                    .owners()
                    .expect("owners")
                    .pairing()
                    .expect("pairing code")
                    .code;
                let pairing = event(
                    "new-message",
                    &message(
                        "p1",
                        OWNER,
                        Some(&format!(" {} ", code.as_str())),
                        false,
                        3_000,
                    ),
                );
                assert_eq!(post_hook(&channel, &path, pairing).await, 204);
                until("the paired reply", || server.remaining() == 0).await;
            })
            .await;

            assert!(recorder.published().is_empty());
            assert!(channel.owners().expect("owners").is_owner(OWNER));
            assert!(!channel.owners().expect("owners").is_owner(STRANGER));
            let reply = requests(server).pop().expect("reply request");
            assert_eq!(reply.1, "/api/v1/message/text?password=pw");
            let reply: serde_json::Value = serde_json::from_str(&reply.2).expect("reply JSON");
            assert_eq!(reply["chatGuid"], chat(OWNER));
            assert_eq!(reply["message"], PAIRED_REPLY);
            assert_eq!(reply["method"], "apple-script");
        });
    }
}

#[test]
fn a_binding_code_pairs_and_strangers_are_ignored() {
    with_gateway(PairingAndStrangers);
}

struct WebhookDeliveries;

impl Scenario for WebhookDeliveries {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S) {
        block_on(async {
            store_configuration(&storage, Some("send_receive"), &[OWNER]).await;
            let mut steps = fresh_start();
            // The oversized delivery asks for a catch-up.
            steps.push(page(
                &[message("old", OWNER, Some("before"), false, 1_000)],
                1,
            ));
            let server = server(steps);
            let recorder = Rc::new(Recorder::default());
            let channel = load(&gateway, &storage, server, &recorder, Some(ADDRESS)).await;
            receiving(&channel, async {
                until("the catch-up", || server.remaining() == 1).await;
                let path = hook_path(&channel);
                let wrong = format!("{HOOK_PATH}/{}", "0".repeat(32));
                let new = |guid: &str, from_me: bool| {
                    event("new-message", &message(guid, OWNER, Some("hi"), from_me, 2_000))
                };

                assert_eq!(post_hook(&channel, &wrong, new("w1", false)).await, 404);
                assert_eq!(post_hook(&channel, HOOK_PATH, new("w2", false)).await, 404);
                let get = WebhookEndpoint(Rc::clone(&channel))
                    .handle(HttpRequest::with_path(HttpMethod::Get, path.clone(), Vec::new()))
                    .await;
                assert_eq!(get.status(), 405);

                let mut oversized = new("big", false);
                oversized.resize(WEBHOOK_BODY_MAX + 1, b' ');
                assert_eq!(post_hook(&channel, &path, oversized).await, 204);
                assert_eq!(channel.book.lost(), 1);
                until("the catch-up after a loss", || server.remaining() == 0).await;

                let typing = event("typing-indicator", r#"{"display":true,"guid":"x"}"#);
                assert_eq!(post_hook(&channel, &path, typing).await, 204);
                assert_eq!(post_hook(&channel, &path, b"not json".to_vec()).await, 204);
                assert_eq!(post_hook(&channel, &path, new("mine", true)).await, 204);
                assert_eq!(
                    channel.deliver(HttpMethod::Post, &path, &new("mine", true)),
                    Delivery::FromMe
                );
                let reaction = event(
                    "new-message",
                    &message("r1", OWNER, Some("Loved “hi”"), false, 2_500)
                        .replace(r#""associatedMessageType":null"#, r#""associatedMessageType":"love""#),
                );
                assert_eq!(post_hook(&channel, &path, reaction).await, 204);
                let attachment = event(
                    "new-message",
                    &message("a1", OWNER, Some("\u{fffc}"), false, 2_600),
                );
                assert_eq!(post_hook(&channel, &path, attachment).await, 204);

                assert_eq!(post_hook(&channel, &path, new("d1", false)).await, 204);
                assert_eq!(
                    channel.deliver(HttpMethod::Post, &path, &new("d1", false)),
                    Delivery::Duplicate
                );
                until("the publish", || !recorder.published().is_empty()).await;
                until("the skips", || channel.book.skipped() == 2).await;
                assert_eq!(
                    channel.deliver(HttpMethod::Post, &path, &new("d1", false)),
                    Delivery::Duplicate,
                    "a handled GUID stays a duplicate"
                );
                let (status, body) = call(
                    &ConfigEndpoint(Rc::clone(&channel)),
                    HttpMethod::Get,
                    "",
                )
                .await;
                assert_eq!(status, 200);
                assert_eq!(
                    body,
                    r#"{"configured":true,"mode":"send_receive","receive":{"state":"receiving"},"owners":{"count":1},"webhook":{"lost":1,"skipped":2}}"#
                );
            })
            .await;

            let published = recorder.published();
            assert_eq!(published.len(), 1);
            assert_eq!(published[0].message_id, "d1");
        });
    }
}

#[test]
fn the_webhook_checks_secret_size_type_sender_and_duplicates() {
    with_gateway(WebhookDeliveries);
}

struct DedupAcrossReconnect;

impl Scenario for DedupAcrossReconnect {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S) {
        block_on(async {
            store_configuration(&storage, Some("send_receive"), &[OWNER]).await;
            let first = message("m1", OWNER, Some("one"), false, 2_000);
            let server_one = server(fresh_start());
            let recorder = Rc::new(Recorder::default());
            let channel = load(&gateway, &storage, server_one, &recorder, Some(ADDRESS)).await;
            let url = receiving(&channel, async {
                until("the catch-up", || server_one.remaining() == 0).await;
                let path = hook_path(&channel);
                assert_eq!(
                    post_hook(&channel, &path, event("new-message", &first)).await,
                    204
                );
                until("the publish", || recorder.published().len() == 1).await;
                until("the cursor write", || !channel.book.is_dirty()).await;
                channel
                    .book
                    .hook()
                    .and_then(|hook| hook.url)
                    .expect("registered URL")
            })
            .await;
            let stored = storage
                .get_bytes(CURSOR_STORAGE_KEY)
                .await
                .expect("read cursor")
                .expect("stored cursor");
            let stored: serde_json::Value = serde_json::from_slice(&stored).expect("cursor JSON");
            assert_eq!(stored["after"], 2_000);
            assert_eq!(stored["seen"], serde_json::json!(["old", "m1"]));
            drop(channel);

            // A reboot: the same storage, a new channel. The webhook is still
            // registered, and the catch-up returns `m1` again (`after` is
            // inclusive) beside a new `m2`.
            let second = message("m2", OWNER, Some("two"), false, 3_000);
            let server_two = server(vec![
                ok(&format!(
                    r#"[{{"id":4,"url":"{url}","events":["new-message"]}}]"#
                )),
                page(&[first, second], 2),
            ]);
            let channel = load(&gateway, &storage, server_two, &recorder, Some(ADDRESS)).await;
            assert_eq!(channel.book.after(), Some(2_000));
            receiving(&channel, async {
                until("the catch-up", || server_two.remaining() == 0).await;
                until("the publish", || recorder.published().len() == 2).await;
            })
            .await;

            let published = recorder.published();
            assert_eq!(published.len(), 2, "m1 is not published twice");
            assert_eq!(published[1].message_id, "m2");
            let requests = requests(server_two);
            assert_eq!(requests.len(), 2, "no second webhook is created");
            assert!(requests[1].2.contains(r#""after":2000"#));
        });
    }
}

#[test]
fn the_cursor_is_restored_and_deduplicates_across_a_reconnect() {
    with_gateway(DedupAcrossReconnect);
}

struct StaleHooks;

impl Scenario for StaleHooks {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S) {
        block_on(async {
            store_configuration(&storage, Some("send_receive"), &[OWNER]).await;
            let recorder = Rc::new(Recorder::default());
            // Mint the secret first, with no address yet: a retryable error.
            let idle = server(Vec::new());
            let channel = load(&gateway, &storage, idle, &recorder, None).await;
            receiving(&channel, async {
                until("the address error", || {
                    channel.receive().state()
                        == ReceiveState::Error("device LAN address unknown".into())
                })
                .await;
            })
            .await;
            assert!(requests(idle).is_empty());
            let secret = channel.book.hook().expect("minted").secret;
            let mut record = channel.book.hook().expect("minted");
            record.url = Some("http://10.0.0.9:8787/api/gateway/bluebubbles/hook/feed".into());
            channel
                .book
                .store_hook(&storage, record)
                .await
                .expect("store");
            drop(channel);

            let other_device = "http://10.0.0.7:8787/api/gateway/bluebubbles/hook/other";
            let old_address = format!("http://10.0.0.8:8787{HOOK_PATH}/{secret}");
            let list = format!(
                r#"[{{"id":1,"url":"http://10.0.0.9:8787/api/gateway/bluebubbles/hook/feed"}},{{"id":2,"url":"{old_address}"}},{{"id":3,"url":"{other_device}"}}]"#
            );
            let old = message("old", OWNER, Some("before"), false, 1_000);
            let server = server(vec![
                page(core::slice::from_ref(&old), 1),
                ok(&list),
                ok(r#"{"id":1}"#),
                ok(r#"{"id":2}"#),
                ok(r#"{"id":5,"url":"x"}"#),
                page(&[old], 1),
            ]);
            let channel = load(&gateway, &storage, server, &recorder, Some(ADDRESS)).await;
            receiving(&channel, async {
                until("the catch-up", || server.remaining() == 0).await;
            })
            .await;
            let requests = requests(server);
            assert_eq!(
                (requests[2].0.as_str(), requests[2].1.as_str()),
                ("DELETE", "/api/v1/webhook/1?password=pw")
            );
            assert_eq!(
                (requests[3].0.as_str(), requests[3].1.as_str()),
                ("DELETE", "/api/v1/webhook/2?password=pw")
            );
            assert_eq!(requests[4].0, "POST", "another device's webhook is kept");
            assert_eq!(channel.book.hook().expect("record").secret, secret);
            assert_eq!(secret, expected_secret());
        });
    }
}

#[test]
fn stale_webhooks_of_this_device_are_replaced_and_an_unknown_address_retries() {
    with_gateway(StaleHooks);
}

struct ServerErrors;

impl Scenario for ServerErrors {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S) {
        block_on(async {
            store_configuration(&storage, Some("send_receive"), &[OWNER]).await;
            let recorder = Rc::new(Recorder::default());
            let refused = ScriptStep::json(401, r#"{"status":401,"message":"Unauthorized"}"#);
            let server = server(vec![refused]);
            let channel = load(&gateway, &storage, server, &recorder, Some(ADDRESS)).await;
            receiving(&channel, async {
                until("the halt", || {
                    channel.receive().state()
                        == ReceiveState::Error("BlueBubbles rejected the password".into())
                })
                .await;
                Timer::after(Duration::from_millis(60)).await;
            })
            .await;
            assert_eq!(requests(server).len(), 1, "a wrong password is not retried");
        });
    }
}

#[test]
fn a_rejected_password_halts_receiving() {
    with_gateway(ServerErrors);
}

struct CatchUpPaging;

impl Scenario for CatchUpPaging {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S) {
        block_on(async {
            store_configuration(&storage, Some("send_receive"), &[OWNER]).await;
            let recorder = Rc::new(Recorder::default());
            let messages: Vec<String> = (0..60_u64)
                .map(|index| message(&format!("c{index}"), OWNER, Some("x"), false, 1_000 + index))
                .collect();
            let oversized = page(
                &[message(
                    "big",
                    OWNER,
                    Some(&"y".repeat(17 * 1024)),
                    false,
                    1,
                )],
                1,
            );
            let mut steps = vec![
                page(&[message("old", OWNER, Some("before"), false, 999)], 1),
                ok(r#"[]"#),
                ok(r#"{"id":1}"#),
                // 62 messages since `after`: the oldest 12 are skipped.
                page(&messages[..10], 62),
                oversized.clone(),
                oversized,
            ];
            for chunk in messages[12..].chunks(3) {
                steps.push(page(chunk, 62));
            }
            steps.push(page(&[], 62));
            let server = server(steps);
            let channel = load(&gateway, &storage, server, &recorder, Some(ADDRESS)).await;
            receiving(&channel, async {
                until("the catch-up", || server.remaining() == 0).await;
                until("the publishes", || recorder.published().len() == 48).await;
            })
            .await;
            let requests = requests(server);
            assert!(requests[4].2.contains(r#""offset":12"#));
            assert!(requests[4].2.contains(r#""limit":10"#));
            assert!(requests[5].2.contains(r#""limit":5"#), "{}", requests[5].2);
            assert!(requests[6].2.contains(r#""limit":3"#), "{}", requests[6].2);
            assert_eq!(channel.book.lost(), 12);
            assert_eq!(recorder.published()[0].message_id, "c12");
            assert_eq!(channel.book.after(), Some(1_059));
        });
    }
}

#[test]
fn the_catch_up_pages_shrinks_and_skips_an_old_backlog() {
    with_gateway(CatchUpPaging);
}

struct ModeSwitches;

impl Scenario for ModeSwitches {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S) {
        block_on(async {
            store_configuration(&storage, Some("send_receive"), &[OWNER]).await;
            let recorder = Rc::new(Recorder::default());
            let mut steps = fresh_start();
            let registered = format!("http://{ADDRESS}:8787{HOOK_PATH}/{}", expected_secret());
            steps.push(ok(&format!(r#"[{{"id":1,"url":"{registered}"}}]"#)));
            steps.push(ok(r#"{}"#));
            let server = server(steps);
            let channel = load(&gateway, &storage, server, &recorder, Some(ADDRESS)).await;
            let mode = ModeEndpoint::new(Rc::clone(&channel));
            receiving(&channel, async {
                until("the catch-up", || server.remaining() == 2).await;
                let path = hook_path(&channel);
                assert_eq!(
                    channel.book.hook().and_then(|hook| hook.url),
                    Some(registered.clone())
                );

                assert_eq!(
                    call(&mode, HttpMethod::Post, r#"{"mode":"send"}"#).await,
                    (204, String::new())
                );
                assert_eq!(channel.receive().state(), ReceiveState::Idle);
                assert!(is_registered(&gateway), "send keeps the channel");
                assert_eq!(server.remaining(), 0, "the webhook was deleted");
                assert_eq!(channel.book.hook().and_then(|hook| hook.url), None);
                assert_eq!(
                    post_hook(&channel, &path, event("new-message", "{}")).await,
                    404
                );
                let (_, status) =
                    call(&ConfigEndpoint(Rc::clone(&channel)), HttpMethod::Get, "").await;
                assert_eq!(
                    status,
                    r#"{"configured":true,"mode":"send","owners":{"count":1}}"#
                );

                assert_eq!(
                    call(&mode, HttpMethod::Post, r#"{"mode":"disabled"}"#).await,
                    (204, String::new())
                );
                assert!(!is_registered(&gateway), "disabled drops the channel");
                assert_eq!(
                    call(&mode, HttpMethod::Post, r#"{"mode":"send"}"#).await,
                    (204, String::new())
                );
                assert!(is_registered(&gateway));
                Timer::after(Duration::from_millis(50)).await;
            })
            .await;
            let requests = requests(server);
            let deleted = &requests[requests.len() - 1];
            assert_eq!(
                (deleted.0.as_str(), deleted.1.as_str()),
                ("DELETE", "/api/v1/webhook/1?password=pw")
            );
            assert_eq!(requests.len(), 6, "send mode makes no requests");
        });
    }
}

#[test]
fn mode_switches_delete_the_webhook_and_follow_registration() {
    with_gateway(ModeSwitches);
}

struct LegacyAndEndpoints;

impl Scenario for LegacyAndEndpoints {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S) {
        block_on(async {
            let recorder = Rc::new(Recorder::default());
            let idle = server(Vec::new());
            let channel = load(&gateway, &storage, idle, &recorder, Some(ADDRESS)).await;
            let config = ConfigEndpoint(Rc::clone(&channel));
            assert_eq!(
                call(&config, HttpMethod::Get, "").await,
                (
                    200,
                    r#"{"configured":false,"mode":"send_receive","receive":{"state":"idle"},"owners":{"count":0},"webhook":{"lost":0,"skipped":0}}"#
                        .into()
                )
            );
            assert_eq!(call(&config, HttpMethod::Put, "").await.0, 405);
            assert_eq!(call(&config, HttpMethod::Post, "{}").await.0, 400);
            // An unconfigured channel holds no owner book.
            let owners = OwnersEndpoint::new(Rc::clone(&channel));
            assert_eq!(
                call(&owners, HttpMethod::Get, "").await,
                (409, r#"{"error":"not_configured"}"#.into())
            );
            drop((config, owners, channel));

            // A configuration stored before modes existed becomes `send`.
            store_configuration(&storage, None, &[]).await;
            storage.delete(MODE_STORAGE_KEY).await.expect("forget mode");
            let channel = load(&gateway, &storage, idle, &recorder, Some(ADDRESS)).await;
            let (status, body) = call(
                &OwnersEndpoint::new(Rc::clone(&channel)),
                HttpMethod::Get,
                "",
            )
            .await;
            assert_eq!(status, 200);
            assert!(
                body.starts_with(r#"{"owners":[],"pairing":{"code":""#),
                "{body}"
            );
            assert_eq!(channel.mode(), ChannelMode::Send);
            assert!(is_registered(&gateway));
            assert!(!channel.receive().is_enabled());
            assert_eq!(
                call(&ConfigEndpoint(Rc::clone(&channel)), HttpMethod::Get, "")
                    .await
                    .1,
                r#"{"configured":true,"mode":"send","owners":{"count":0}}"#
            );
            assert!(storage
                .get_bytes(HOOK_STORAGE_KEY)
                .await
                .expect("read")
                .is_none());
        });
    }
}

#[test]
fn legacy_configurations_send_only_and_endpoints_answer_json() {
    with_gateway(LegacyAndEndpoints);
}

struct Configuration {
    occupied: bool,
}

impl Scenario for Configuration {
    fn run<S: PluginStorage>(self, gateway: Rc<IMessageGateway>, storage: S) {
        block_on(async {
            let occupant: Rc<dyn MessageChannel> = Rc::new(Occupant);
            let _occupied = self
                .occupied
                .then(|| gateway.register(occupant).expect("occupy"));
            let recorder = Rc::new(Recorder::default());
            let idle = server(Vec::new());
            let channel = load(&gateway, &storage, idle, &recorder, Some(ADDRESS)).await;
            let config = ConfigEndpoint(Rc::clone(&channel));
            let body = r#"{"server_url":"https://blue.example","password":"secret"}"#;
            let (status, response) = call(&config, HttpMethod::Post, body).await;
            let stored = storage
                .get_bytes(CONFIGURATION_STORAGE_KEY)
                .await
                .expect("read");
            if self.occupied {
                assert_eq!(status, 422);
                assert_eq!(response, r#"{"error":"registration_failed"}"#);
                assert!(stored.is_none());
                assert!(!channel.configured());
            } else {
                assert_eq!(status, 204);
                assert!(stored.is_some());
                assert!(channel.configured());
                assert!(channel.receive().is_enabled(), "new channels receive");
            }
        });
    }
}

#[test]
fn accepted_configuration_is_reported_and_registered() {
    with_gateway(Configuration { occupied: false });
}

#[test]
fn rejected_channel_registration_rolls_back_persisted_configuration() {
    with_gateway(Configuration { occupied: true });
}

#[test]
fn stored_configuration_round_trips_every_field() -> Result<(), serde_json::Error> {
    let config = ConfigRequest {
        server_url: "https://blue.example".into(),
        password: "secret".into(),
        use_private_api: false,
        stream_edit_min_delta_bytes: 64,
        stream_max_edits: 7,
    };

    let restored =
        crate::channel::decode_configuration(&crate::channel::encode_configuration(&config)?)?;

    assert_eq!(restored.server_url, config.server_url);
    assert_eq!(restored.password, config.password);
    assert_eq!(restored.use_private_api, config.use_private_api);
    assert_eq!(
        restored.stream_edit_min_delta_bytes,
        config.stream_edit_min_delta_bytes
    );
    assert_eq!(restored.stream_max_edits, config.stream_max_edits);
    Ok(())
}

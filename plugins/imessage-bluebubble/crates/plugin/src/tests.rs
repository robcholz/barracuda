//! The channel against the real Workflow, HTTP, WebServer, and Gateway on a
//! loopback network, a scripted BlueBubbles server there, and real Plugin
//! storage.
//!
//! Each test runs on an executor of its own, in a process of its own: the
//! Gateway's receive buffers are static, and Workflow must run to record what
//! the channel publishes. Webhook deliveries and the channel's routes go
//! through the WebServer over an in-memory connection.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    missing_docs
)]

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use std::sync::mpsc::{sync_channel, SyncSender};

use barracuda_http_plugin::HttpPlugin;
use barracuda_imessage_gateway_plugin::{
    ChannelError, ChannelFuture, ChannelMode, IMessageGatewayPlugin, MessageChannel, Operation,
    ReceiveState, SendMessageRequest, SendReceipt, PAIRED_REPLY,
};
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_platform_test::{
    install_global_memory_vfs, loopback_network, memory_partition, LoopbackNetwork,
    MemoryPartition, ScriptStep, ScriptedServer,
};
use barracuda_plugin::manager::{PluginDeclaration, PluginManager};
use barracuda_webserver_plugin::{HttpMethod, WebRouteRegistration, WebServerPlugin};
use barracuda_workflow_plugin::{
    workflow_action_schema_inline, WorkflowActionFuture, WorkflowActionHandler,
    WorkflowActionRegistry, WorkflowActionSchema, WorkflowPlugin, WorkflowService,
};
use embassy_executor::{Executor, Spawner};
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Timer};
use rusty_fork::rusty_fork_test;
use serde_json::{json, Value};

use super::*;
use crate::channel::CONFIGURATION_STORAGE_KEY;
use crate::state::{CURSOR_STORAGE_KEY, HOOK_STORAGE_KEY};
use crate::webhook::{Delivery, WEBHOOK_BODY_MAX};

const TIMING: ReceiveTiming = ReceiveTiming {
    initial_backoff: Duration::from_millis(20),
    max_backoff: Duration::from_millis(40),
    slot_retry: Duration::from_millis(20),
};
const OWNER: &str = "+15550001111";
const STRANGER: &str = "+15559990000";
/// The loopback stack's address, which the webhook URL names.
const ADDRESS: &str = "10.0.0.1";
/// Port of the scripted BlueBubbles server on the loopback network.
const PORT: u16 = 9000;
const SERVER_URL: &str = "http://10.0.0.1:9000";
/// The server's listeners. The gateway opens a connection per request and
/// drops it without a FIN, so each one holds a listener until the server's
/// retransmission draws a reset; the catch-up's burst of requests needs one
/// listener each.
const LISTENERS: usize = 28;
/// Stored mode key.
const MODE_STORAGE_KEY: &str = "mode";

type Channel<S> = BlueBubblesChannel<S>;

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

// --- Workflow recording what the channel publishes ---------------------------

/// Records every `gateway.message.received` a Workflow hands it.
struct Record(Rc<RefCell<Vec<Value>>>);

impl WorkflowActionHandler for Record {
    type Request = Value;
    type Response = Value;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
        "test.record",
        r#"{"type":"object","properties":{"route":{"type":"object"},"message_id":{"type":"string"},"text":{"type":"string"},"attachments":{"type":"array"}}}"#,
        r#"{"type":"object"}"#
    );

    fn invoke(&self, request: Value) -> WorkflowActionFuture<'_, Value> {
        self.0.borrow_mut().push(request);
        Box::pin(async { Ok(json!({})) })
    }
}

/// Hands the whole Event, attachments included, to `test.record`.
const RECORD_WORKFLOW: &str = r#"{
    "id": "record-inbound",
    "match": {"event": "gateway.message.received"},
    "steps": [{"call": "test.record"}]
}"#;

struct Recorder(Rc<RefCell<Vec<Value>>>);

impl PluginDeclaration for Recorder {
    const ID: &'static str = "inbound-recorder";
    const DEPENDS_ON: &'static [&'static str] = &["workflow"];
}

impl Plugin for Recorder {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let service = context.require::<WorkflowService>("workflow")?;
        context.retain(
            actions
                .add_action(Record(Rc::clone(&self.0)))
                .map_err(PluginError::registration)?,
        );
        embassy_futures::block_on(service.load_transient(RECORD_WORKFLOW))
            .map_err(PluginError::registration)?;
        Ok(())
    }
}

// --- The bench ---------------------------------------------------------------

/// What a scenario works with: the capabilities the channel Plugin gets, its
/// storage, the scripted server, and what Workflow recorded.
struct Harness<S: PluginStorage> {
    gateway: Rc<IMessageGateway>,
    webserver: Rc<WebServer>,
    storage: S,
    bench: Bench,
}

/// A channel built as the Plugin's `register` builds it, with its routes
/// served and its receive runtime, which runs only inside [`Self::receiving`].
struct Loaded<S: PluginStorage> {
    channel: Rc<Channel<S>>,
    runtime: Cell<Option<ReceiveRuntime>>,
    _routes: [WebRouteRegistration; 2],
}

impl<S: PluginStorage> Harness<S> {
    /// Loads the channel from storage, on the loopback stack's address.
    fn load(&self) -> Loaded<S> {
        self.load_at(lan_address(self.bench.stack))
    }

    /// Loads the channel on a device that has no LAN address yet.
    fn load_without_address(&self) -> Loaded<S> {
        self.load_at(Box::new(|| None))
    }

    fn load_at(&self, address: LocalAddress) -> Loaded<S> {
        let (channel, runtime) = build_channel(
            self.storage.clone(),
            Rc::clone(&self.gateway),
            SharedEntropy::new(Pattern),
            address,
            TIMING,
        )
        .expect("load the channel");
        let routes = serve_routes(&self.webserver, Rc::clone(&channel)).expect("serve the routes");
        Loaded {
            channel,
            runtime: Cell::new(Some(runtime)),
            _routes: routes,
        }
    }

    fn server(&self) -> &ScriptedServer {
        &self.bench.server
    }

    /// Appends `steps` to the server's answers.
    fn script(&self, steps: impl IntoIterator<Item = ScriptStep>) {
        for step in steps {
            self.bench.server.push(step);
        }
    }

    /// Requests the server answered from the `from`-th on: method, path with
    /// query, and body.
    fn requests_from(&self, from: usize) -> Vec<(String, String, String)> {
        self.bench
            .server
            .requests()
            .into_iter()
            .skip(from)
            .map(|raw| {
                let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
                let mut line = head.lines().next().unwrap_or_default().split_whitespace();
                let method = line.next().unwrap_or_default().to_string();
                let path = line.next().unwrap_or_default().to_string();
                (method, path, body.to_string())
            })
            .collect()
    }

    fn requests(&self) -> Vec<(String, String, String)> {
        self.requests_from(0)
    }

    /// What Workflow recorded, in order.
    fn published(&self) -> Vec<Value> {
        self.bench.records.borrow().clone()
    }

    /// Requests `path` on the WebServer; returns the status and body.
    async fn request(&self, method: &str, path: &str, body: &[u8]) -> (u16, String) {
        let mut input = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        input.extend_from_slice(body);
        let output = routes::send(&self.webserver, input).await;
        let split = output
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("response head");
        let head = core::str::from_utf8(&output[..split]).expect("ASCII head");
        let status = head[9..12].parse().expect("status code");
        let body = String::from_utf8(output[split + 4..].to_vec()).expect("UTF-8 body");
        (status, body)
    }

    /// Delivers a webhook `body` to `path`, as the BlueBubbles server does.
    async fn post_hook(&self, path: &str, body: Vec<u8>) -> u16 {
        self.request("POST", path, &body).await.0
    }

    /// `GET /api/gateway/bluebubbles/status`.
    async fn status(&self) -> String {
        let (status, body) = self
            .request("GET", &format!("{CONFIG_API_PATH}/status"), b"")
            .await;
        assert_eq!(status, 200);
        body
    }

    async fn set_mode(&self, mode: &str) -> (u16, String) {
        let body = format!(r#"{{"mode":"{mode}"}}"#);
        self.request("POST", &format!("{CONFIG_API_PATH}/mode"), body.as_bytes())
            .await
    }

    async fn owners(&self) -> (u16, String) {
        self.request("GET", &format!("{CONFIG_API_PATH}/owners"), b"")
            .await
    }
}

impl<S: PluginStorage> Loaded<S> {
    /// Runs the channel's receive runtime while `test` runs.
    async fn receiving<F: Future>(&self, test: F) -> F::Output {
        let mut runtime = self.runtime.take().expect("receive runtime");
        let output = match select(&mut runtime, test).await {
            Either::First(()) => panic!("the receive runtime ended"),
            Either::Second(output) => output,
        };
        self.runtime.set(Some(runtime));
        output
    }
}

/// Waits up to five seconds for `condition`.
async fn until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        Timer::after(Duration::from_millis(2)).await;
    }
}

/// Lets Workflow run whatever the channel published so far.
async fn settle() {
    Timer::after(Duration::from_millis(50)).await;
}

/// One test's steps: given the harness during registration, where Plugin
/// storage exists, they run once every Plugin started.
trait Scenario: Send + 'static {
    fn run<S: PluginStorage>(self, harness: Harness<S>) -> Steps;
}

/// A scenario's steps, waiting to run.
type Steps = Pin<Box<dyn Future<Output = ()>>>;

/// The loopback stack, the scripted server on it, what Workflow recorded,
/// and where the probe leaves the scenario's steps.
#[derive(Clone)]
struct Bench {
    stack: Stack<'static>,
    server: ScriptedServer,
    records: Rc<RefCell<Vec<Value>>>,
    pending: Rc<RefCell<Option<Steps>>>,
}

/// Hands the scenario its harness during registration.
struct Probe<Sc> {
    scenario: Option<Sc>,
    bench: Bench,
}

impl<Sc> PluginDeclaration for Probe<Sc> {
    const ID: &'static str = "bluebubbles-probe";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway", "webserver"];
}

impl<Sc: Scenario> Plugin for Probe<Sc> {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let harness = Harness {
            gateway: context.require::<IMessageGateway>("imessage-gateway")?,
            webserver: context.require::<WebServer>("webserver")?,
            storage: context.storage().clone(),
            bench: self.bench.clone(),
        };
        if let Some(scenario) = self.scenario.take() {
            self.bench.pending.replace(Some(scenario.run(harness)));
        }
        Ok(())
    }
}

/// Registers the probe of one scenario.
type RegisterProbe = Box<dyn FnOnce(&mut PluginManager<MemoryPartition>, Bench) + Send>;

/// Runs `scenario` on an executor of its own, beside Workflow, HTTP,
/// WebServer, and the Gateway on a loopback network, and the scripted
/// BlueBubbles server there.
fn run(scenario: impl Scenario) {
    let register: RegisterProbe = Box::new(move |manager, bench| {
        manager
            .register(Probe {
                scenario: Some(scenario),
                bench,
            })
            .expect("register the probe");
    });
    let (done, finished) = sync_channel(1);
    std::thread::Builder::new()
        .name("bluebubbles-scenario".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner.spawn(scenario_task(spawner, register, done).expect("spawn the scenario"));
            });
        })
        .expect("spawn the executor thread");
    finished
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the scenario failed or timed out");
}

#[embassy_executor::task]
async fn run_network(network: LoopbackNetwork) {
    network.run().await;
}

#[embassy_executor::task]
async fn run_server(server: ScriptedServer, stack: Stack<'static>) {
    server.run::<LISTENERS>(stack, PORT).await;
}

#[embassy_executor::task]
async fn scenario_task(spawner: Spawner, register: RegisterProbe, done: SyncSender<()>) {
    install_global_memory_vfs().await.expect("install test VFS");
    let partition = memory_partition(64 * 1024)
        .await
        .expect("test database region");
    let mut manager = PluginManager::open(partition)
        .await
        .expect("open Plugin storage");
    manager.install_vfs(barracuda_vfs::global_namespace().await);
    manager.install_task_spawner(spawner);
    let network = loopback_network();
    let stack = network.stack();
    let mut context = plugin_context(stack);
    let bench = Bench {
        stack,
        // Every BlueBubbles request opens its own connection.
        server: ScriptedServer::new([]),
        records: Rc::new(RefCell::new(Vec::new())),
        pending: Rc::new(RefCell::new(None)),
    };
    spawner.spawn(run_network(network).expect("spawn the network"));
    spawner.spawn(run_server(bench.server.clone(), stack).expect("spawn the server"));
    register_services(&mut manager, &mut context);
    manager
        .register(Recorder(Rc::clone(&bench.records)))
        .expect("register the recorder");
    register(&mut manager, bench.clone());
    manager.start().expect("start the Plugins");
    let scenario = bench.pending.take().expect("the scenario");
    scenario.await;
    done.send(()).expect("report the outcome");
    // The executor thread outlives the test; keep its Plugins running.
    core::mem::forget(manager);
}

/// A context on the loopback `stack`.
fn plugin_context(stack: Stack<'static>) -> PluginContext {
    let info = barracuda_plugin::api::TargetIdentity::new(
        barracuda_plugin::api::PlatformInfo::new("test", "test", "test-arch", "hosted"),
        barracuda_plugin::api::BoardInfo::new(
            "test-board",
            barracuda_plugin::api::Hardware::new("test-chip"),
        ),
    );
    PluginContext::new(info, stack)
}

/// Registers the Plugins the channel runs on.
fn register_services(manager: &mut PluginManager<MemoryPartition>, context: &mut PluginContext) {
    manager
        .register(WorkflowPlugin::new(context))
        .expect("register Workflow Plugin");
    manager
        .register(HttpPlugin::new(context))
        .expect("register HTTP Plugin");
    manager
        .register(WebServerPlugin::new(context))
        .expect("register WebServer Plugin");
    // Only the Gateway draws from the entropy that mints pairing codes.
    context.entropy = SharedEntropy::new(Pattern);
    manager
        .register(IMessageGatewayPlugin::new(context))
        .expect("register IMessage Gateway Plugin");
}

// --- Scripted server answers -------------------------------------------------

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

async fn store_configuration<S: PluginStorage>(storage: &S, mode: Option<&str>, owners: &[&str]) {
    let configuration =
        format!(r#"{{"server_url":"{SERVER_URL}","password":"pw","use_private_api":false}}"#);
    storage
        .put(CONFIGURATION_STORAGE_KEY, configuration.as_bytes())
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

// --- Scenarios ---------------------------------------------------------------

struct OwnerMessageIsPublished;

impl Scenario for OwnerMessageIsPublished {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            store_configuration(&h.storage, Some("send_receive"), &[OWNER]).await;
            h.script(fresh_start());
            let loaded = h.load();
            let channel = &loaded.channel;
            assert!(is_registered(&h.gateway));
            loaded
                .receiving(async {
                    until("the catch-up", || h.server().remaining() == 0).await;
                    assert_eq!(channel.receive().state(), ReceiveState::Receiving);
                    let path = hook_path(channel);
                    let body = event(
                        "new-message",
                        &message("m1", OWNER, Some("hello"), false, 2_000),
                    );
                    assert_eq!(h.post_hook(&path, body).await, 204);
                    until("the publish", || !h.published().is_empty()).await;
                    settle().await;
                })
                .await;

            let published = h.published();
            assert_eq!(published.len(), 1, "`old` was the cursor start");
            // No `thread_id`: the route is the chat alone.
            assert_eq!(
                published[0],
                json!({"route": {"channel": "bluebubbles", "conversation_id": chat(OWNER)},
                       "message_id": "m1", "text": "hello"})
            );

            let secret = channel.book.hook().expect("record").secret;
            assert_eq!(secret.len(), 32);
            let url = format!("http://{ADDRESS}:8787/api/gateway/bluebubbles/hook/{secret}");
            let requests = h.requests();
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
            assert!(requests[3].2.contains(r#""with":["chat","attachment"]"#));
            assert_eq!(channel.book.hook().and_then(|hook| hook.url), Some(url));
        })
    }
}

rusty_fork_test! {
    #[test]
    fn an_owner_message_is_published_on_the_chat_route() {
        run(OwnerMessageIsPublished);
    }
}

/// `message` with `attachments` (a JSON array) in place of none.
fn with_attachments(message: &str, attachments: &str) -> String {
    message.replace(
        r#""attachments":[]"#,
        &format!(r#""attachments":{attachments}"#),
    )
}

struct AttachmentsAreDownloaded;

impl Scenario for AttachmentsAreDownloaded {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            store_configuration(&h.storage, Some("send_receive"), &[OWNER]).await;
            h.script(fresh_start());
            h.script([
                ScriptStep::response(200, "image/jpeg", b"JPEGDATA", usize::MAX),
                ScriptStep::json(
                    404,
                    r#"{"status":404,"message":"Attachment does not exist!"}"#,
                ),
            ]);
            let loaded = h.load();
            let channel = &loaded.channel;
            loaded
                .receiving(async {
                    until("the catch-up", || h.server().remaining() == 2).await;
                    let path = hook_path(channel);
                    let theirs = with_attachments(
                        &message("s1", STRANGER, Some("\u{fffc}"), false, 2_000),
                        r#"[{"guid":"at_theirs","mimeType":"image/jpeg","transferName":"x.jpeg","totalBytes":8}]"#,
                    );
                    assert_eq!(
                        h.post_hook(&path, event("new-message", &theirs)).await,
                        204
                    );
                    until("the stranger to be ignored", || {
                        channel.owners().expect("owners").ignored() == 1
                    })
                    .await;
                    assert_eq!(h.requests().len(), 4, "a stranger's file is never fetched");

                    let mine = with_attachments(
                        &message("m1", OWNER, Some("\u{fffc}look\u{fffc}\u{fffc}"), false, 3_000),
                        r#"[{"guid":"at_A","uti":"public.jpeg","mimeType":"image/jpeg","transferName":"IMG_1.jpeg","totalBytes":8},
                            {"guid":"at_B","mimeType":"image/heic","transferName":"IMG_2.HEIC","totalBytes":10},
                            {"guid":"at_C","mimeType":"video/quicktime","transferName":"clip.mov","totalBytes":1099511627776}]"#,
                    );
                    assert_eq!(h.post_hook(&path, event("new-message", &mine)).await, 204);
                    until("the publish", || !h.published().is_empty()).await;
                    settle().await;
                })
                .await;

            let published = h.published();
            assert_eq!(published.len(), 1);
            let message = &published[0];
            assert_eq!(message["message_id"], "m1");
            assert_eq!(
                message["text"], "look\n[file not received]\n[video too large, not received]",
                "the marks are gone and each missing file is a note"
            );
            let attachments = message["attachments"].as_array().expect("attachments");
            assert_eq!(attachments.len(), 1);
            assert_eq!(attachments[0]["kind"], "image");
            assert_eq!(attachments[0]["mime_type"], "image/jpeg");
            assert_eq!(attachments[0]["filename"], "IMG_1.jpeg");
            assert_eq!(attachments[0]["size"], 8);
            let stored = attachments[0]["path"].as_str().expect("path");
            assert!(
                stored.starts_with("/workspace/cache/gateway/bluebubbles/")
                    && stored.ends_with(".jpg"),
                "{stored}"
            );
            // The oversized video is never asked for.
            let downloads: Vec<_> = h
                .requests_from(4)
                .into_iter()
                .map(|(method, path, _body)| (method, path))
                .collect();
            assert_eq!(
                downloads,
                [
                    (
                        "GET".to_string(),
                        "/api/v1/attachment/at_A/download?password=pw".to_string()
                    ),
                    (
                        "GET".to_string(),
                        "/api/v1/attachment/at_B/download?password=pw".to_string()
                    ),
                ]
            );
        })
    }
}

rusty_fork_test! {
    #[test]
    fn an_owners_attachments_are_downloaded_and_a_missing_one_is_a_note() {
        run(AttachmentsAreDownloaded);
    }
}

struct PairingAndStrangers;

impl Scenario for PairingAndStrangers {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            store_configuration(&h.storage, Some("send_receive"), &[]).await;
            h.script(fresh_start());
            h.script([ok(r#"{"guid":"sent-1"}"#)]);
            let loaded = h.load();
            let channel = &loaded.channel;
            loaded
                .receiving(async {
                    until("the catch-up", || h.server().remaining() == 1).await;
                    let path = hook_path(channel);
                    let stranger = event(
                        "new-message",
                        &message("s1", STRANGER, Some("let me in"), false, 2_000),
                    );
                    assert_eq!(h.post_hook(&path, stranger).await, 204);
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
                    assert_eq!(h.post_hook(&path, pairing).await, 204);
                    until("the paired reply", || h.server().remaining() == 0).await;
                    settle().await;
                })
                .await;

            assert!(h.published().is_empty());
            assert!(channel.owners().expect("owners").is_owner(OWNER));
            assert!(!channel.owners().expect("owners").is_owner(STRANGER));
            let reply = h.requests().pop().expect("reply request");
            assert_eq!(reply.1, "/api/v1/message/text?password=pw");
            let reply: Value = serde_json::from_str(&reply.2).expect("reply JSON");
            assert_eq!(reply["chatGuid"], chat(OWNER));
            assert_eq!(reply["message"], PAIRED_REPLY);
            assert_eq!(reply["method"], "apple-script");
        })
    }
}

rusty_fork_test! {
    #[test]
    fn a_binding_code_pairs_and_strangers_are_ignored() {
        run(PairingAndStrangers);
    }
}

struct WebhookDeliveries;

impl Scenario for WebhookDeliveries {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            store_configuration(&h.storage, Some("send_receive"), &[OWNER]).await;
            h.script(fresh_start());
            // The oversized delivery asks for a catch-up.
            h.script([page(
                &[message("old", OWNER, Some("before"), false, 1_000)],
                1,
            )]);
            let loaded = h.load();
            let channel = &loaded.channel;
            loaded
                .receiving(async {
                    until("the catch-up", || h.server().remaining() == 1).await;
                    let path = hook_path(channel);
                    let wrong = format!("{HOOK_PATH}/{}", "0".repeat(32));
                    let new = |guid: &str, from_me: bool| {
                        event(
                            "new-message",
                            &message(guid, OWNER, Some("hi"), from_me, 2_000),
                        )
                    };

                    assert_eq!(h.post_hook(&wrong, new("w1", false)).await, 404);
                    assert_eq!(h.post_hook(HOOK_PATH, new("w2", false)).await, 404);
                    assert_eq!(h.request("GET", &path, b"").await.0, 405);

                    let mut oversized = new("big", false);
                    oversized.resize(WEBHOOK_BODY_MAX + 1, b' ');
                    assert_eq!(h.post_hook(&path, oversized).await, 204);
                    assert_eq!(channel.book.lost(), 1);
                    until("the catch-up after a loss", || h.server().remaining() == 0).await;

                    let typing = event("typing-indicator", r#"{"display":true,"guid":"x"}"#);
                    assert_eq!(h.post_hook(&path, typing).await, 204);
                    assert_eq!(h.post_hook(&path, b"not json".to_vec()).await, 204);
                    assert_eq!(h.post_hook(&path, new("mine", true)).await, 204);
                    assert_eq!(
                        channel.deliver(HttpMethod::Post, &path, &new("mine", true)),
                        Delivery::FromMe
                    );
                    let reaction = event(
                        "new-message",
                        &message("r1", OWNER, Some("Loved “hi”"), false, 2_500).replace(
                            r#""associatedMessageType":null"#,
                            r#""associatedMessageType":"love""#,
                        ),
                    );
                    assert_eq!(h.post_hook(&path, reaction).await, 204);
                    let attachment = event(
                        "new-message",
                        &message("a1", OWNER, Some("\u{fffc}"), false, 2_600),
                    );
                    assert_eq!(h.post_hook(&path, attachment).await, 204);

                    assert_eq!(h.post_hook(&path, new("d1", false)).await, 204);
                    assert_eq!(
                        channel.deliver(HttpMethod::Post, &path, &new("d1", false)),
                        Delivery::Duplicate
                    );
                    until("the publish", || !h.published().is_empty()).await;
                    until("the skips", || channel.book.skipped() == 2).await;
                    assert_eq!(
                        channel.deliver(HttpMethod::Post, &path, &new("d1", false)),
                        Delivery::Duplicate,
                        "a handled GUID stays a duplicate"
                    );
                    assert_eq!(
                        h.status().await,
                        r#"{"configured":true,"mode":"send_receive","receive":{"state":"receiving"},"owners":{"count":1},"webhook":{"lost":1,"skipped":2}}"#
                    );
                    settle().await;
                })
                .await;

            let published = h.published();
            assert_eq!(published.len(), 1);
            assert_eq!(published[0]["message_id"], "d1");
        })
    }
}

rusty_fork_test! {
    #[test]
    fn the_webhook_checks_secret_size_type_sender_and_duplicates() {
        run(WebhookDeliveries);
    }
}

struct DedupAcrossReconnect;

impl Scenario for DedupAcrossReconnect {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            store_configuration(&h.storage, Some("send_receive"), &[OWNER]).await;
            let first = message("m1", OWNER, Some("one"), false, 2_000);
            h.script(fresh_start());
            let loaded = h.load();
            let channel = &loaded.channel;
            let url = loaded
                .receiving(async {
                    until("the catch-up", || h.server().remaining() == 0).await;
                    let path = hook_path(channel);
                    assert_eq!(h.post_hook(&path, event("new-message", &first)).await, 204);
                    until("the publish", || h.published().len() == 1).await;
                    until("the cursor write", || !channel.book.is_dirty()).await;
                    channel
                        .book
                        .hook()
                        .and_then(|hook| hook.url)
                        .expect("registered URL")
                })
                .await;
            let stored = h
                .storage
                .get_bytes(CURSOR_STORAGE_KEY)
                .await
                .expect("read cursor")
                .expect("stored cursor");
            let stored: Value = serde_json::from_slice(&stored).expect("cursor JSON");
            assert_eq!(stored["after"], 2_000);
            assert_eq!(stored["seen"], json!(["old", "m1"]));
            drop(loaded);

            // A reboot: the same storage, a new channel. The webhook is still
            // registered, and the catch-up returns `m1` again (`after` is
            // inclusive) beside a new `m2`.
            let second = message("m2", OWNER, Some("two"), false, 3_000);
            let reboot = h.server().requests().len();
            h.script([
                ok(&format!(
                    r#"[{{"id":4,"url":"{url}","events":["new-message"]}}]"#
                )),
                page(&[first, second], 2),
            ]);
            let loaded = h.load();
            assert_eq!(loaded.channel.book.after(), Some(2_000));
            loaded
                .receiving(async {
                    until("the catch-up", || h.server().remaining() == 0).await;
                    until("the publish", || h.published().len() == 2).await;
                    settle().await;
                })
                .await;

            let published = h.published();
            assert_eq!(published.len(), 2, "m1 is not published twice");
            assert_eq!(published[1]["message_id"], "m2");
            let requests = h.requests_from(reboot);
            assert_eq!(requests.len(), 2, "no second webhook is created");
            assert!(requests[1].2.contains(r#""after":2000"#));
        })
    }
}

rusty_fork_test! {
    #[test]
    fn the_cursor_is_restored_and_deduplicates_across_a_reconnect() {
        run(DedupAcrossReconnect);
    }
}

struct StaleHooks;

impl Scenario for StaleHooks {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            store_configuration(&h.storage, Some("send_receive"), &[OWNER]).await;
            // Mint the secret first, with no address yet: a retryable error.
            let loaded = h.load_without_address();
            let channel = &loaded.channel;
            loaded
                .receiving(async {
                    until("the address error", || {
                        channel.receive().state()
                            == ReceiveState::Error("device LAN address unknown".into())
                    })
                    .await;
                })
                .await;
            assert!(h.requests().is_empty());
            let secret = channel.book.hook().expect("minted").secret;
            let mut record = channel.book.hook().expect("minted");
            record.url = Some("http://10.0.0.9:8787/api/gateway/bluebubbles/hook/feed".into());
            channel
                .book
                .store_hook(&h.storage, record)
                .await
                .expect("store");
            drop(loaded);

            let other_device = "http://10.0.0.7:8787/api/gateway/bluebubbles/hook/other";
            let old_address = format!("http://10.0.0.8:8787{HOOK_PATH}/{secret}");
            let list = format!(
                r#"[{{"id":1,"url":"http://10.0.0.9:8787/api/gateway/bluebubbles/hook/feed"}},{{"id":2,"url":"{old_address}"}},{{"id":3,"url":"{other_device}"}}]"#
            );
            let old = message("old", OWNER, Some("before"), false, 1_000);
            h.script([
                page(core::slice::from_ref(&old), 1),
                ok(&list),
                ok(r#"{"id":1}"#),
                ok(r#"{"id":2}"#),
                ok(r#"{"id":5,"url":"x"}"#),
                page(&[old], 1),
            ]);
            let loaded = h.load();
            loaded
                .receiving(async {
                    until("the catch-up", || h.server().remaining() == 0).await;
                })
                .await;
            let requests = h.requests();
            assert_eq!(
                (requests[2].0.as_str(), requests[2].1.as_str()),
                ("DELETE", "/api/v1/webhook/1?password=pw")
            );
            assert_eq!(
                (requests[3].0.as_str(), requests[3].1.as_str()),
                ("DELETE", "/api/v1/webhook/2?password=pw")
            );
            assert_eq!(requests[4].0, "POST", "another device's webhook is kept");
            assert_eq!(loaded.channel.book.hook().expect("record").secret, secret);
            assert_eq!(secret, expected_secret());
        })
    }
}

rusty_fork_test! {
    #[test]
    fn stale_webhooks_of_this_device_are_replaced_and_an_unknown_address_retries() {
        run(StaleHooks);
    }
}

struct ServerErrors;

impl Scenario for ServerErrors {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            store_configuration(&h.storage, Some("send_receive"), &[OWNER]).await;
            h.script([ScriptStep::json(
                401,
                r#"{"status":401,"message":"Unauthorized"}"#,
            )]);
            let loaded = h.load();
            let channel = &loaded.channel;
            loaded
                .receiving(async {
                    until("the halt", || {
                        channel.receive().state()
                            == ReceiveState::Error("BlueBubbles rejected the password".into())
                    })
                    .await;
                    Timer::after(Duration::from_millis(60)).await;
                })
                .await;
            assert_eq!(h.requests().len(), 1, "a wrong password is not retried");
        })
    }
}

rusty_fork_test! {
    #[test]
    fn a_rejected_password_halts_receiving() {
        run(ServerErrors);
    }
}

struct CatchUpPaging;

impl Scenario for CatchUpPaging {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            store_configuration(&h.storage, Some("send_receive"), &[OWNER]).await;
            let messages: Vec<String> = (0..60_u64)
                .map(|index| message(&format!("c{index}"), OWNER, Some("x"), false, 1_000 + index))
                .collect();
            let oversized = || {
                page(
                    &[message(
                        "big",
                        OWNER,
                        Some(&"y".repeat(17 * 1024)),
                        false,
                        1,
                    )],
                    1,
                )
            };
            h.script([
                page(&[message("old", OWNER, Some("before"), false, 999)], 1),
                ok(r#"[]"#),
                ok(r#"{"id":1}"#),
                // 62 messages since `after`: the oldest 12 are skipped.
                page(&messages[..10], 62),
                oversized(),
                oversized(),
            ]);
            for chunk in messages[12..].chunks(3) {
                h.script([page(chunk, 62)]);
            }
            h.script([page(&[], 62)]);
            let loaded = h.load();
            let channel = &loaded.channel;
            loaded
                .receiving(async {
                    until("the catch-up", || h.server().remaining() == 0).await;
                    until("the publishes", || h.published().len() == 48).await;
                    settle().await;
                })
                .await;
            let requests = h.requests();
            assert!(requests[4].2.contains(r#""offset":12"#));
            assert!(requests[4].2.contains(r#""limit":10"#));
            assert!(requests[5].2.contains(r#""limit":5"#), "{}", requests[5].2);
            assert!(requests[6].2.contains(r#""limit":3"#), "{}", requests[6].2);
            assert_eq!(channel.book.lost(), 12);
            let published = h.published();
            assert_eq!(published.len(), 48);
            assert_eq!(published[0]["message_id"], "c12");
            assert_eq!(channel.book.after(), Some(1_059));
        })
    }
}

rusty_fork_test! {
    /// Makes 23 sequential requests, each on a connection of its own.
    #[test]
    fn the_catch_up_pages_shrinks_and_skips_an_old_backlog() {
        run(CatchUpPaging);
    }
}

struct ModeSwitches;

impl Scenario for ModeSwitches {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            store_configuration(&h.storage, Some("send_receive"), &[OWNER]).await;
            let registered = format!("http://{ADDRESS}:8787{HOOK_PATH}/{}", expected_secret());
            h.script(fresh_start());
            h.script([
                ok(&format!(r#"[{{"id":1,"url":"{registered}"}}]"#)),
                ok(r#"{}"#),
            ]);
            let loaded = h.load();
            let channel = &loaded.channel;
            loaded
                .receiving(async {
                    until("the catch-up", || h.server().remaining() == 2).await;
                    let path = hook_path(channel);
                    assert_eq!(
                        channel.book.hook().and_then(|hook| hook.url),
                        Some(registered.clone())
                    );

                    assert_eq!(h.set_mode("send").await, (204, String::new()));
                    assert_eq!(channel.receive().state(), ReceiveState::Idle);
                    assert!(is_registered(&h.gateway), "send keeps the channel");
                    assert_eq!(h.server().remaining(), 0, "the webhook was deleted");
                    assert_eq!(channel.book.hook().and_then(|hook| hook.url), None);
                    assert_eq!(h.post_hook(&path, event("new-message", "{}")).await, 404);
                    assert_eq!(
                        h.status().await,
                        r#"{"configured":true,"mode":"send","owners":{"count":1}}"#
                    );

                    assert_eq!(h.set_mode("disabled").await, (204, String::new()));
                    assert!(!is_registered(&h.gateway), "disabled drops the channel");
                    assert_eq!(h.set_mode("send").await, (204, String::new()));
                    assert!(is_registered(&h.gateway));
                    Timer::after(Duration::from_millis(50)).await;
                })
                .await;
            let requests = h.requests();
            let deleted = &requests[requests.len() - 1];
            assert_eq!(
                (deleted.0.as_str(), deleted.1.as_str()),
                ("DELETE", "/api/v1/webhook/1?password=pw")
            );
            assert_eq!(requests.len(), 6, "send mode makes no requests");
        })
    }
}

rusty_fork_test! {
    #[test]
    fn mode_switches_delete_the_webhook_and_follow_registration() {
        run(ModeSwitches);
    }
}

struct LegacyAndEndpoints;

impl Scenario for LegacyAndEndpoints {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            let loaded = h.load();
            assert_eq!(
                h.status().await,
                r#"{"configured":false,"mode":"send_receive","receive":{"state":"idle"},"owners":{"count":0},"webhook":{"lost":0,"skipped":0}}"#
            );
            // The configuration path takes changes only; its state is under `/status`.
            assert_eq!(h.request("GET", CONFIG_API_PATH, b"").await.0, 405);
            assert_eq!(h.request("PUT", CONFIG_API_PATH, b"").await.0, 405);
            assert_eq!(h.request("POST", CONFIG_API_PATH, b"{}").await.0, 400);
            // An unconfigured channel holds no owner book.
            assert_eq!(
                h.owners().await,
                (409, r#"{"error":"not_configured"}"#.into())
            );
            drop(loaded);

            // A configuration stored before modes existed becomes `send`.
            store_configuration(&h.storage, None, &[]).await;
            h.storage
                .delete(MODE_STORAGE_KEY)
                .await
                .expect("forget mode");
            let loaded = h.load();
            let channel = &loaded.channel;
            let (status, body) = h.owners().await;
            assert_eq!(status, 200);
            assert!(
                body.starts_with(r#"{"owners":[],"pairing":{"code":""#),
                "{body}"
            );
            assert_eq!(channel.mode(), ChannelMode::Send);
            assert!(is_registered(&h.gateway));
            assert!(!channel.receive().is_enabled());
            assert_eq!(
                h.status().await,
                r#"{"configured":true,"mode":"send","owners":{"count":0}}"#
            );
            assert!(h
                .storage
                .get_bytes(HOOK_STORAGE_KEY)
                .await
                .expect("read")
                .is_none());
        })
    }
}

rusty_fork_test! {
    #[test]
    fn legacy_configurations_send_only_and_endpoints_answer_json() {
        run(LegacyAndEndpoints);
    }
}

struct Configuration {
    occupied: bool,
}

impl Scenario for Configuration {
    fn run<S: PluginStorage>(self, h: Harness<S>) -> Steps {
        Box::pin(async move {
            let occupant: Rc<dyn MessageChannel> = Rc::new(Occupant);
            let _occupied = self
                .occupied
                .then(|| h.gateway.register(occupant).expect("occupy"));
            let loaded = h.load();
            let channel = &loaded.channel;
            let body = br#"{"server_url":"https://blue.example","password":"secret"}"#;
            let (status, response) = h.request("POST", CONFIG_API_PATH, body).await;
            let stored = h
                .storage
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
        })
    }
}

rusty_fork_test! {
    #[test]
    fn accepted_configuration_is_reported_and_registered() {
        run(Configuration { occupied: false });
    }

    #[test]
    fn rejected_channel_registration_rolls_back_persisted_configuration() {
        run(Configuration { occupied: true });
    }
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

/// Serves one in-memory connection through the WebServer's routes.
mod routes {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::future::Future;

    use barracuda_webserver_plugin::WebServer;
    use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
    use picoserve::io::Socket;
    use picoserve::time::{Duration, TimeoutError, Timer};

    /// Request buffer of the connection: large enough for a webhook body over
    /// `WEBHOOK_BODY_MAX` to reach the endpoint.
    const HTTP_BUFFER_BYTES: usize = 16 * 1024;

    /// Returns the WebServer's raw answer to the raw request `input`.
    pub(super) async fn send(server: &WebServer, input: Vec<u8>) -> Vec<u8> {
        let mut output = Output(Vec::new());
        let mut buffer = vec![0; HTTP_BUFFER_BYTES];
        server
            .serve_connection(
                TestTimer,
                &mut buffer,
                TestSocket {
                    input: Input(input, 0),
                    output: &mut output,
                },
            )
            .await
            .expect("serve the connection");
        output.0
    }

    struct Input(Vec<u8>, usize);

    impl ErrorType for Input {
        type Error = ErrorKind;
    }

    impl Read for Input {
        async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, ErrorKind> {
            let rest = &self.0[self.1..];
            let count = bytes.len().min(rest.len());
            bytes[..count].copy_from_slice(&rest[..count]);
            self.1 += count;
            Ok(count)
        }
    }

    struct Output(Vec<u8>);

    impl ErrorType for Output {
        type Error = ErrorKind;
    }

    impl Write for Output {
        async fn write(&mut self, bytes: &[u8]) -> Result<usize, ErrorKind> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        async fn flush(&mut self) -> Result<(), ErrorKind> {
            Ok(())
        }
    }

    impl picoserve::io::Write for Output {
        async fn write_with<F: FnOnce(&mut [u8]) -> (usize, R), R>(
            &mut self,
            f: F,
        ) -> Result<R, ErrorKind> {
            let mut scratch = [0; 128];
            let (count, result) = f(&mut scratch);
            self.0.extend_from_slice(&scratch[..count]);
            Ok(result)
        }
    }

    struct TestSocket<'a> {
        input: Input,
        output: &'a mut Output,
    }

    impl Socket<()> for TestSocket<'_> {
        type Error = ErrorKind;
        type ReadHalf<'a>
            = &'a mut Input
        where
            Self: 'a;
        type WriteHalf<'a>
            = &'a mut Output
        where
            Self: 'a;

        fn split(&mut self) -> (Self::ReadHalf<'_>, Self::WriteHalf<'_>) {
            (&mut self.input, self.output)
        }

        async fn abort<T: Timer<()>>(
            self,
            _: &picoserve::Timeouts,
            _: &mut T,
        ) -> Result<(), picoserve::Error<ErrorKind>> {
            Ok(())
        }

        async fn shutdown<T: Timer<()>>(
            self,
            _: &picoserve::Timeouts,
            _: &mut T,
        ) -> Result<(), picoserve::Error<ErrorKind>> {
            Ok(())
        }
    }

    struct TestTimer;

    impl Timer<()> for TestTimer {
        async fn delay(&self, _: Duration) {
            core::future::pending::<()>().await;
        }

        async fn run_with_timeout<F: Future>(
            &self,
            _: Duration,
            future: F,
        ) -> Result<F::Output, TimeoutError> {
            Ok(future.await)
        }
    }
}

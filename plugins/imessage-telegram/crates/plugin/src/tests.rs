//! The Telegram channel against scripted Bot API servers, real Plugin
//! storage, and the real Gateway and Workflow.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs
)]

use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use core::cell::{Cell, RefCell};
use std::sync::mpsc::{sync_channel, SyncSender};

use barracuda_http_plugin::HttpPlugin;
use barracuda_imessage_gateway_plugin::ReceiveSlots;
use barracuda_imessage_gateway_plugin::{
    ChannelControl, ChannelError, ChannelFuture, IMessageGatewayPlugin, MessageChannel, Operation,
    ReceiveControl, ReceiveState, SendMessageRequest, SendReceipt, PAIRED_REPLY,
};
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_platform_test::{
    install_global_memory_vfs, loopback_network, memory_partition, MemoryPartition, ScriptStep,
    ScriptedRoute, ScriptedServer,
};
use barracuda_plugin::api::SharedEntropy;
use barracuda_plugin::manager::{PluginDeclaration, PluginManager};
use barracuda_webserver_plugin::{WebServer, WebServerPlugin};
use barracuda_workflow_plugin::{
    workflow_action_schema_inline, WorkflowActionFuture, WorkflowActionHandler,
    WorkflowActionRegistry, WorkflowActionSchema, WorkflowPlugin, WorkflowService,
};
use core::future::Future;
use core::pin::Pin;
use embassy_executor::{Executor, Spawner};
use embassy_futures::join::join;
use embassy_futures::select::{select, Either};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use futures_lite::future::block_on;
use rusty_fork::rusty_fork_test;
use serde_json::{json, Value};

use super::*;
use crate::receive::{Cursor, CURSOR_STORAGE_KEY, MAX_BODY_BYTES};

const TOKEN: &str = "123:secret";
/// Port of the scripted Bot API on the loopback network.
const PORT: u16 = 9000;
const CONFIG: &str =
    r#"{"token":"123:secret","api_base":"http://10.0.0.1:9000","draft_min_delta_bytes":24}"#;
const TIMING: ReceiveTiming = ReceiveTiming {
    initial_backoff: Duration::from_millis(20),
    max_backoff: Duration::from_millis(80),
    slot_retry: Duration::from_millis(20),
};
/// Backoff long enough that an error state stays put while a test reads it.
const SLOW_TIMING: ReceiveTiming = ReceiveTiming {
    initial_backoff: Duration::from_secs(30),
    max_backoff: Duration::from_secs(30),
    slot_retry: Duration::from_secs(30),
};
const PAIRING_CODE: &str = "123456";
const WEBHOOK_CONFLICT: &str = "Conflict: can't use getUpdates method while webhook is active; use deleteWebhook to delete the webhook first";
const OTHER_POLLER: &str =
    "Conflict: terminated by other getUpdates request; make sure that only one bot instance is running";

type Channel<Storage> = TelegramChannel<Storage>;

/// Entropy that counts up from a seed, so the first pairing code is the seed.
#[derive(Clone)]
struct Sequence(portable_atomic_util::Arc<portable_atomic::AtomicU32>);

impl Entropy for Sequence {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        let value = self.0.fetch_add(1, portable_atomic::Ordering::Relaxed);
        bytes.copy_from_slice(&value.to_le_bytes()[..bytes.len()]);
        Ok(())
    }
}

/// Entropy whose first pairing code is [`PAIRING_CODE`].
fn pairing_entropy() -> SharedEntropy {
    SharedEntropy::new(Sequence(portable_atomic_util::Arc::new(
        portable_atomic::AtomicU32::new(123_456),
    )))
}

fn ok(result: Value) -> ScriptStep {
    ScriptStep::json(200, &json!({"ok": true, "result": result}).to_string())
}

fn refused(status: u16, description: &str, retry_after: Option<u32>) -> ScriptStep {
    let mut body = json!({"ok": false, "error_code": status, "description": description});
    if let Some(seconds) = retry_after {
        body["parameters"] = json!({"retry_after": seconds});
    }
    ScriptStep::json(status, &body.to_string())
}

/// A long poll the server holds open.
fn idle() -> ScriptStep {
    ScriptStep::pending_after_headers(200, "application/json")
}

fn text_update(update_id: i64, sender: i64, text: &str) -> Value {
    json!({
        "update_id": update_id,
        "message": {
            "message_id": update_id * 10,
            "date": 1,
            "from": {"id": sender, "is_bot": false, "first_name": "User", "username": format!("user{sender}")},
            "chat": {"id": sender, "type": "private"},
            "text": text,
        }
    })
}

/// Request targets the server answered, in order.
fn targets(requests: Vec<String>) -> Vec<String> {
    requests
        .into_iter()
        .map(|request| {
            request
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

struct OccupiedTelegramChannel;

impl MessageChannel for OccupiedTelegramChannel {
    fn channel(&self) -> &str {
        "telegram"
    }

    fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::SendMessage)) })
    }
}

/// What the probe stores before building the channel, and what the Bot API
/// answers.
struct Setup {
    configured: bool,
    mode: Option<&'static str>,
    stored: Vec<(&'static str, Vec<u8>)>,
    /// Answers to `getUpdates` and `deleteWebhook`, in order.
    receive: Vec<ScriptStep>,
    /// Answers to `sendMessage`, in order.
    send: Vec<ScriptStep>,
    /// Answers to `getFile`, in order.
    files: Vec<ScriptStep>,
    /// Answers to file downloads, in order.
    downloads: Vec<ScriptStep>,
    /// Whether other receivers hold every Gateway slot.
    hold_slot: bool,
    occupied: bool,
    timing: ReceiveTiming,
}

impl Setup {
    fn configured(mode: &'static str) -> Self {
        Self {
            configured: true,
            mode: Some(mode),
            ..Self::unconfigured()
        }
    }

    fn unconfigured() -> Self {
        Self {
            configured: false,
            mode: None,
            stored: Vec::new(),
            receive: Vec::new(),
            send: Vec::new(),
            files: Vec::new(),
            downloads: Vec::new(),
            hold_slot: false,
            occupied: false,
            timing: TIMING,
        }
    }
}

/// The loopback network and the scripted Bot API on it, both running
/// whenever a test waits.
struct Bench {
    /// Answers `getUpdates` and `deleteWebhook`.
    polls: ScriptedServer,
    /// Answers `sendMessage`.
    sends: ScriptedRoute,
    /// Answers `getFile`.
    files: ScriptedRoute,
    /// Answers file downloads.
    downloads: ScriptedRoute,
    driver: RefCell<Pin<Box<dyn Future<Output = ()>>>>,
}

impl Bench {
    fn new(network: barracuda_platform_test::LoopbackNetwork, setup: &mut Setup) -> Rc<Self> {
        let stack = network.stack();
        let polls = ScriptedServer::new(core::mem::take(&mut setup.receive));
        let sends = polls.route("/sendMessage", core::mem::take(&mut setup.send));
        let files = polls.route("/getFile", core::mem::take(&mut setup.files));
        let downloads = polls.route("/file/bot", core::mem::take(&mut setup.downloads));
        let serving = polls.clone();
        // Enough listeners for the poll beside back-to-back `getFile`s and
        // downloads, each on a fresh connection.
        let driver: Pin<Box<dyn Future<Output = ()>>> = Box::pin(async move {
            let _stopped = join(network.run(), serving.run::<8>(stack, PORT)).await;
        });
        Rc::new(Self {
            polls,
            sends,
            files,
            downloads,
            driver: RefCell::new(driver),
        })
    }

    /// Runs `future` to its end while the network and the Bot API run.
    fn drive<F: Future>(&self, future: F) -> F::Output {
        let mut driver = self.driver.borrow_mut();
        match block_on(select(driver.as_mut(), future)) {
            Either::First(()) => panic!("the loopback network stopped"),
            Either::Second(output) => output,
        }
    }
}

/// A route's answer.
struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    const fn status(&self) -> u16 {
        self.status
    }

    fn body(&self) -> Option<&[u8]> {
        Some(&self.body)
    }
}

/// The built channel, its routes, and the scripted Bot API.
struct Harness<Storage: PluginStorage> {
    channel: Rc<Channel<Storage>>,
    runtime: RefCell<Option<ReceiveRuntime>>,
    bench: Rc<Bench>,
    webserver: Rc<WebServer>,
    /// Receivers holding every Gateway slot, when the setup asks for it.
    held: RefCell<Vec<ReceiveControl<ReceiveSlots>>>,
    /// The Gateway's slot count.
    capacity: usize,
    gateway: Rc<IMessageGateway>,
    storage: Storage,
    occupied: RefCell<Option<barracuda_imessage_gateway_plugin::MessageChannelRegistration>>,
}

impl<Storage: PluginStorage> Harness<Storage> {
    /// Drives the receive runtime until `done` holds; fails after 5 s.
    fn run_until(&self, what: &str, done: impl Fn() -> bool) {
        let mut runtime = self.runtime.borrow_mut();
        let runtime = runtime.as_mut().expect("receive runtime");
        let waited = self.bench.drive(select(
            runtime,
            with_timeout(Duration::from_secs(5), async {
                while !done() {
                    Timer::after_millis(2).await;
                }
            }),
        ));
        assert!(
            matches!(waited, Either::Second(Ok(()))),
            "timed out waiting for {what}"
        );
    }

    /// Drives the receive runtime for `duration`.
    fn run_for(&self, duration: Duration) {
        let mut runtime = self.runtime.borrow_mut();
        let runtime = runtime.as_mut().expect("receive runtime");
        let _elapsed = self.bench.drive(select(runtime, Timer::after(duration)));
    }

    fn state(&self) -> ReceiveState {
        self.channel.receive().state()
    }

    /// Targets of the receive requests so far.
    fn polls(&self) -> Vec<String> {
        targets(self.bench.polls.requests())
    }

    /// Requests the channel's routes on the WebServer.
    fn request(&self, method: &str, path: &str, body: &str) -> Reply {
        let mut input = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        input.extend_from_slice(body.as_bytes());
        let output = self.bench.drive(routes::send(&self.webserver, input));
        let split = output
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("response head");
        let head = core::str::from_utf8(&output[..split]).expect("ASCII head");
        Reply {
            status: head[9..12].parse().expect("status code"),
            body: output[split + 4..].to_vec(),
        }
    }

    fn status(&self) -> Value {
        let reply = self.request("GET", &format!("{CONFIG_API_PATH}/status"), "");
        assert_eq!(reply.status(), 200);
        serde_json::from_slice(&reply.body).expect("status JSON")
    }

    fn configure(&self, body: &str) -> Reply {
        self.request("POST", CONFIG_API_PATH, body)
    }

    fn set_mode(&self, mode: &str) -> Reply {
        let body = format!(r#"{{"mode":"{mode}"}}"#);
        self.request("POST", &format!("{CONFIG_API_PATH}/mode"), &body)
    }

    fn stored(&self, key: &str) -> Option<Vec<u8>> {
        block_on(self.storage.get_bytes(key)).expect("read storage")
    }

    /// Whether a `telegram` channel is registered with the Gateway.
    fn registered(&self) -> bool {
        let probe: Rc<dyn MessageChannel> = Rc::new(OccupiedTelegramChannel);
        self.gateway.register(probe).is_err()
    }
}

/// One test's steps, run inside Plugin registration where scoped storage
/// exists. Returning the runtime asks the probe to spawn it in `start`.
trait Scenario: 'static {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime>;
}

struct Probe<S> {
    setup: Option<Setup>,
    bench: Rc<Bench>,
    scenario: S,
    runtime: Option<ReceiveRuntime>,
    completed: Rc<Cell<bool>>,
}

impl<S> PluginDeclaration for Probe<S> {
    const ID: &'static str = "telegram-probe";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway", "webserver"];
}

impl<S: Scenario> Plugin for Probe<S> {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let setup = self.setup.take().expect("setup");
        let storage = context.storage().clone();
        if setup.configured {
            block_on(storage.put(CONFIGURATION_STORAGE_KEY, CONFIG.as_bytes()))?;
        }
        if let Some(mode) = setup.mode {
            block_on(storage.put("mode", format!("\"{mode}\"").as_bytes()))?;
        }
        for (key, value) in &setup.stored {
            block_on(storage.put(key, value.as_slice()))?;
        }
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let webserver = context.require::<WebServer>("webserver")?;
        let occupied = setup.occupied.then(|| {
            let occupied: Rc<dyn MessageChannel> = Rc::new(OccupiedTelegramChannel);
            gateway
                .register(occupied)
                .expect("occupy the telegram channel")
        });
        let capacity = gateway.receive_control().capacity();
        let held = if setup.hold_slot {
            (0..capacity)
                .map(|_| {
                    let control = gateway.receive_control();
                    control.set_enabled(true).expect("a free slot");
                    control
                })
                .collect()
        } else {
            Vec::new()
        };
        let (channel, runtime) = build_channel(storage.clone(), Rc::clone(&gateway), setup.timing)?;
        let config = ConfigEndpoint {
            channel: Rc::clone(&channel),
        };
        context.retain(
            gateway
                .serve_channel(CONFIG_API_PATH, Rc::clone(&channel), config)
                .map_err(PluginError::registration)?,
        );
        self.runtime = self.scenario.run(Harness {
            channel,
            runtime: RefCell::new(Some(runtime)),
            bench: Rc::clone(&self.bench),
            webserver,
            held: RefCell::new(held),
            capacity,
            gateway,
            storage,
            occupied: RefCell::new(occupied),
        });
        self.completed.set(true);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        if let Some(runtime) = self.runtime.take() {
            let task = telegram_receive_task(runtime, context.task_token())
                .map_err(PluginError::registration)?;
            context.task_spawner()?.spawn(task);
        }
        Ok(())
    }
}

/// A context on `network`'s stack.
fn plugin_context(network: &barracuda_platform_test::LoopbackNetwork) -> PluginContext {
    let info = barracuda_plugin::api::TargetIdentity::new(
        barracuda_plugin::api::PlatformInfo::new("test", "test", "test-arch", "hosted"),
        barracuda_plugin::api::BoardInfo::new(
            "test-board",
            barracuda_plugin::api::Hardware::new("test-chip"),
        ),
    );
    PluginContext::new(info, network.stack())
}

/// Registers Workflow, HTTP, WebServer, the Gateway, and a probe running
/// `scenario` on a loopback network, without starting any task.
fn run(mut setup: Setup, scenario: impl Scenario) {
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("create test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let network = loopback_network();
    let mut context = plugin_context(&network);
    let bench = Bench::new(network, &mut setup);
    let completed = Rc::new(Cell::new(false));
    register_services(&mut manager, &mut context);
    manager
        .register(Probe {
            setup: Some(setup),
            bench,
            scenario,
            runtime: None,
            completed: Rc::clone(&completed),
        })
        .expect("run the Telegram scenario");
    assert!(completed.get());
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
    // Only the Gateway draws from the entropy that mints the first code.
    context.entropy = pairing_entropy();
    manager
        .register(IMessageGatewayPlugin::new(context))
        .expect("register IMessage Gateway Plugin");
}

// --- Configuration -------------------------------------------------------

struct AcceptedConfiguration;

impl Scenario for AcceptedConfiguration {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        assert_eq!(
            harness.status(),
            json!({"configured": false, "mode": "send_receive",
                   "receive": {"state": "idle", "slots": {"in_use": 0, "capacity": harness.capacity}},
                   "owners": {"count": 0}})
        );
        assert_eq!(
            harness.stored("mode").as_deref(),
            Some(&br#""send_receive""#[..])
        );
        assert!(!harness.registered());
        // Nothing receive-related is loaded before the channel is configured.
        assert!(harness.channel.owners().is_none());
        assert!(harness.channel.cursor.get().is_none());

        assert_eq!(
            harness
                .configure(r#"{"token":"secret","api_base":"http://10.0.0.1:9000"}"#)
                .status(),
            204
        );

        assert!(harness.stored(CONFIGURATION_STORAGE_KEY).is_some());
        assert!(harness.registered());
        assert!(harness.channel.owners().is_some());
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        assert_eq!(
            harness.status(),
            json!({"configured": true, "mode": "send_receive",
                   "receive": {"state": "receiving", "slots": {"in_use": 1, "capacity": harness.capacity}},
                   "owners": {"count": 0}})
        );
        assert!(harness.polls()[0].starts_with("/botsecret/getUpdates?timeout=50&limit=8"));
        // The configuration path takes changes only; its state is under `/status`.
        for method in [HttpMethod::Get, HttpMethod::Delete] {
            let response = block_on(
                ConfigEndpoint {
                    channel: Rc::clone(&harness.channel),
                }
                .handle(HttpRequest::new(method, Vec::new())),
            );
            assert_eq!(response.status(), 405);
        }
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_new_configuration_registers_and_starts_receiving() {
        run(
            Setup {
                receive: vec![ok(json!([])), idle()],
                ..Setup::unconfigured()
            },
            AcceptedConfiguration,
        );
    }
}

struct RejectedConfiguration;

impl Scenario for RejectedConfiguration {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        let response = harness.configure(r#"{"token":"secret"}"#);
        assert_eq!(response.status(), 422);
        assert_eq!(
            response.body(),
            Some(&br#"{"error":"registration_failed"}"#[..])
        );
        assert!(harness.stored(CONFIGURATION_STORAGE_KEY).is_none());
        assert_eq!(harness.status()["configured"], json!(false));
        harness.run_for(Duration::from_millis(50));
        assert!(harness.polls().is_empty());
        drop(harness.occupied.take());
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn rejected_channel_registration_is_registration_failed() {
        run(
            Setup {
                occupied: true,
                ..Setup::unconfigured()
            },
            RejectedConfiguration,
        );
    }
}

struct LegacyConfiguration;

impl Scenario for LegacyConfiguration {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        assert_eq!(
            harness.status(),
            json!({"configured": true, "mode": "send", "owners": {"count": 0}})
        );
        assert_eq!(harness.stored("mode").as_deref(), Some(&br#""send""#[..]));
        assert!(harness.registered());
        harness.run_for(Duration::from_millis(50));
        assert!(harness.polls().is_empty());
        assert_eq!(harness.channel.receive().in_use(), 0);
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_configuration_stored_before_modes_sends_only() {
        run(
            Setup {
                configured: true,
                ..Setup::unconfigured()
            },
            LegacyConfiguration,
        );
    }
}

#[test]
fn stored_configuration_round_trips_every_field() -> Result<(), serde_json::Error> {
    let config = ConfigRequest {
        token: "secret".into(),
        api_base: "https://telegram.example".into(),
        draft_min_delta_bytes: 31,
    };

    let bytes = encode_configuration(&config)?;
    let restored = decode_configuration(&bytes)?;

    assert_eq!(restored.token, config.token);
    assert_eq!(restored.api_base, config.api_base);
    assert_eq!(restored.draft_min_delta_bytes, config.draft_min_delta_bytes);
    Ok(())
}

// --- Modes and slots ------------------------------------------------------

struct ModeSwitches;

impl Scenario for ModeSwitches {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        assert_eq!(harness.channel.receive().in_use(), 1);
        assert!(harness.registered());

        assert_eq!(harness.set_mode("disabled").status(), 204);
        harness.run_until("the slot to free", || {
            harness.channel.receive().in_use() == 0
        });
        assert_eq!(harness.state(), ReceiveState::Idle);
        assert!(!harness.registered());
        assert_eq!(
            harness.status(),
            json!({"configured": true, "mode": "disabled", "owners": {"count": 0}})
        );

        assert_eq!(harness.set_mode("send").status(), 204);
        harness.run_for(Duration::from_millis(50));
        assert!(harness.registered());
        assert_eq!(harness.channel.receive().in_use(), 0);
        assert_eq!(harness.polls().len(), 2);
        assert_eq!(harness.stored("mode").as_deref(), Some(&br#""send""#[..]));

        assert_eq!(harness.set_mode("send_receive").status(), 204);
        harness.run_until("a second session", || harness.polls().len() == 4);
        harness.run_until("receiving again", || {
            harness.state() == ReceiveState::Receiving
        });
        assert_eq!(harness.bench.polls.connect_count(), 2);
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn modes_start_stop_and_register_the_channel() {
        run(
            Setup {
                receive: vec![ok(json!([])), idle(), ok(json!([])), idle()],
                ..Setup::configured("send_receive")
            },
            ModeSwitches,
        );
    }
}

struct NoSlotWaits;

impl Scenario for NoSlotWaits {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_for(Duration::from_millis(20));
        assert_eq!(harness.state(), ReceiveState::NoSlot);
        assert_eq!(
            harness.status(),
            json!({"configured": true, "mode": "send_receive",
                   "receive": {"state": "no_slot", "capacity": harness.capacity, "slots": {"in_use": harness.capacity, "capacity": harness.capacity}},
                   "owners": {"count": 0}})
        );
        let response = harness.set_mode("send_receive");
        assert_eq!(response.status(), 409);
        assert_eq!(
            response.body(),
            Some(format!(r#"{{"error":"no_slot","capacity":{}}}"#, harness.capacity).as_bytes())
        );

        // The slot retry is 30 s: only the freed-slot wake can start it now.
        let freed = Instant::now();
        drop(harness.held.take());
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        assert!(freed.elapsed() < Duration::from_secs(1));
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_full_pool_reports_no_slot_and_starts_when_a_slot_frees() {
        run(
            Setup {
                receive: vec![ok(json!([])), idle()],
                hold_slot: true,
                timing: SLOW_TIMING,
                ..Setup::configured("send_receive")
            },
            NoSlotWaits,
        );
    }
}

// --- Cursor ----------------------------------------------------------------

struct CursorRestored;

impl Scenario for CursorRestored {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("two polls", || harness.polls().len() == 2);
        let polls = harness.polls();
        assert!(polls[0].ends_with("&offset=42"), "{}", polls[0]);
        assert!(polls[1].ends_with("&offset=44"), "{}", polls[1]);
        assert_eq!(
            harness.stored(CURSOR_STORAGE_KEY).as_deref(),
            Some(&b"44"[..])
        );
        assert_eq!(harness.channel.owners().expect("owners").ignored(), 0);
        assert!(harness.bench.sends.requests().is_empty());
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn the_cursor_is_restored_acknowledged_and_stored() {
        run(
            Setup {
                stored: vec![(CURSOR_STORAGE_KEY, b"42".to_vec())],
                receive: vec![
                    ok(json!([
                        {"update_id": 42, "message": {"message_id": 1, "from": {"id": 7, "first_name": "A"},
                                                      "chat": {"id": 7}, "sticker": {"file_id": "x"}}},
                        {"update_id": 43, "edited_message": {"message_id": 1}},
                    ])),
                    idle(),
                ],
                ..Setup::configured("send_receive")
            },
            CursorRestored,
        );
    }
}

struct OversizedBatch;

impl Scenario for OversizedBatch {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("three polls", || harness.polls().len() == 3);
        let polls = harness.polls();
        assert!(polls[0].contains("&limit=8&"), "{}", polls[0]);
        assert!(polls[1].contains("&limit=1&"), "{}", polls[1]);
        assert!(!polls[1].contains("offset"), "{}", polls[1]);
        assert!(polls[2].contains("&limit=8&"), "{}", polls[2]);
        assert!(polls[2].ends_with("&offset=501"), "{}", polls[2]);
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn an_oversized_update_is_refetched_alone_then_skipped() {
        // The Bot API writes `update_id` first, which skipping relies on.
        let body = format!(
            r#"{{"ok":true,"result":[{{"update_id":500,"message":{{"message_id":5000,"from":{{"id":7,"first_name":"A"}},"chat":{{"id":7}},"text":"{}"}}}}]}}"#,
            "x".repeat(MAX_BODY_BYTES)
        );
        let oversized = || ScriptStep::json(200, &body);
        run(
            Setup {
                receive: vec![oversized(), oversized(), idle()],
                ..Setup::configured("send_receive")
            },
            OversizedBatch,
        );
    }
}

// --- Service errors ---------------------------------------------------------

struct WebhookConflict;

impl Scenario for WebhookConflict {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("the second conflict", || {
            matches!(harness.state(), ReceiveState::Error(_))
        });
        assert_eq!(
            harness.state(),
            ReceiveState::Error(WEBHOOK_CONFLICT.into())
        );
        let polls = harness.polls();
        assert_eq!(polls.len(), 4, "{polls:?}");
        assert!(polls[0].contains("/getUpdates?"));
        assert_eq!(polls[1], format!("/bot{TOKEN}/deleteWebhook"));
        assert!(polls[2].contains("/getUpdates?"));
        assert!(polls[3].contains("/getUpdates?"));
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_webhook_is_deleted_once_per_entry_into_receiving() {
        run(
            Setup {
                receive: vec![
                    refused(409, WEBHOOK_CONFLICT, None),
                    ok(json!(true)),
                    ok(json!([])),
                    refused(409, WEBHOOK_CONFLICT, None),
                ],
                timing: SLOW_TIMING,
                ..Setup::configured("send_receive")
            },
            WebhookConflict,
        );
    }
}

struct OtherPoller;

impl Scenario for OtherPoller {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("the conflict", || {
            matches!(harness.state(), ReceiveState::Error(_))
        });
        assert_eq!(
            harness.status()["receive"],
            json!({"state": "error", "message": OTHER_POLLER, "slots": {"in_use": 1, "capacity": harness.capacity}})
        );
        assert_eq!(harness.polls().len(), 1);
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn another_poller_is_reported_and_backed_off() {
        run(
            Setup {
                receive: vec![refused(409, OTHER_POLLER, None)],
                timing: SLOW_TIMING,
                ..Setup::configured("send_receive")
            },
            OtherPoller,
        );
    }
}

struct RateLimited;

impl Scenario for RateLimited {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("the 429", || harness.polls().len() == 1);
        let limited = Instant::now();
        harness.run_until("the retry", || harness.bench.polls.connect_count() == 2);
        assert!(limited.elapsed() >= Duration::from_millis(900));
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_rate_limit_waits_for_retry_after() {
        run(
            Setup {
                receive: vec![
                    refused(429, "Too Many Requests: retry after 1", Some(1)),
                    ok(json!([])),
                    idle(),
                ],
                ..Setup::configured("send_receive")
            },
            RateLimited,
        );
    }
}

struct BadToken;

impl Scenario for BadToken {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("the refusal", || {
            matches!(harness.state(), ReceiveState::Error(_))
        });
        assert_eq!(harness.state(), ReceiveState::Error("Unauthorized".into()));
        // Halted: no retry however long the backoff would have been.
        harness.run_for(Duration::from_millis(200));
        assert_eq!(harness.polls().len(), 1);

        // A new bot restarts the loop and forgets the old bot's cursor.
        assert_eq!(
            harness
                .configure(r#"{"token":"456:other","api_base":"http://10.0.0.1:9000"}"#)
                .status(),
            204
        );
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        let polls = harness.polls();
        assert!(polls[0].ends_with("&offset=77"), "{}", polls[0]);
        assert!(
            polls[1].starts_with("/bot456:other/getUpdates?"),
            "{}",
            polls[1]
        );
        assert!(!polls[1].contains("offset"), "{}", polls[1]);
        assert!(harness.stored(CURSOR_STORAGE_KEY).is_none());
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_rejected_token_halts_until_reconfigured() {
        run(
            Setup {
                stored: vec![(CURSOR_STORAGE_KEY, b"77".to_vec())],
                receive: vec![refused(401, "Unauthorized", None), ok(json!([])), idle()],
                ..Setup::configured("send_receive")
            },
            BadToken,
        );
    }
}

// --- Owners, publishing, and dedup with Workflow running -------------------

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

/// What the end-to-end test reads back from the running channel.
struct Snapshot {
    owners: Vec<String>,
    labels: Vec<Option<String>>,
    ignored: u32,
    offset: Option<i64>,
}

type Snapshots = Rc<RefCell<Option<Box<dyn Fn() -> Snapshot>>>>;

struct Started(Snapshots);

impl Scenario for Started {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        let pairing = harness
            .channel
            .owners()
            .expect("owners")
            .pairing()
            .expect("pairing code");
        assert_eq!(pairing.code.as_str(), PAIRING_CODE);
        let channel = Rc::clone(&harness.channel);
        self.0.replace(Some(Box::new(move || Snapshot {
            owners: channel
                .owners()
                .expect("owners")
                .owners()
                .iter()
                .map(|owner| owner.id().into())
                .collect(),
            labels: channel
                .owners()
                .expect("owners")
                .owners()
                .iter()
                .map(|owner| owner.label().map(Into::into))
                .collect(),
            ignored: channel.owners().expect("owners").ignored(),
            offset: channel.cursor.get().and_then(Cursor::offset),
        })));
        // Keeps the scripted networks reachable from the snapshot's test.
        NETWORKS.with(|networks| {
            networks.replace(Some((
                harness.bench.polls.clone(),
                harness.bench.sends.clone(),
            )))
        });
        harness.runtime.take()
    }
}

std::thread_local! {
    static NETWORKS: RefCell<Option<(ScriptedServer, ScriptedRoute)>> =
        const { RefCell::new(None) };
}

/// Runs the bench's network and Bot API on the executor.
#[allow(
    clippy::await_holding_refcell_ref,
    reason = "the end-to-end test only waits on the executor, never through `Bench::drive`"
)]
#[embassy_executor::task]
async fn run_bench(bench: Rc<Bench>) {
    let mut driver = bench.driver.borrow_mut();
    driver.as_mut().await;
}

/// One end-to-end test's steps, run on the executor.
type Steps = fn(Spawner) -> Pin<Box<dyn Future<Output = Result<(), String>>>>;

#[embassy_executor::task]
async fn end_to_end(spawner: Spawner, steps: Steps, done: SyncSender<Result<(), String>>) {
    let outcome = steps(spawner).await;
    let _sent = done.send(outcome);
}

/// Runs `steps` on an executor thread of their own and fails the test with
/// their error.
fn on_executor(steps: Steps) {
    let (done, result) = sync_channel(1);
    std::thread::Builder::new()
        .name("telegram-end-to-end".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner.spawn(end_to_end(spawner, steps, done).expect("spawn the end-to-end test"));
            });
        })
        .expect("spawn the executor thread");
    result
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the end-to-end test timed out")
        .expect("the end-to-end test failed");
}

/// Plugins running a receiving channel with `owner` stored, the messages
/// Workflow published, and the scripted Bot API.
struct Running {
    manager: PluginManager<MemoryPartition>,
    records: Rc<RefCell<Vec<Value>>>,
    bench: Rc<Bench>,
}

/// Registers and starts every Plugin, with the channel receiving.
async fn start_receiving(spawner: Spawner, mut setup: Setup) -> Result<Running, String> {
    install_global_memory_vfs()
        .await
        .map_err(|error| error.to_string())?;
    let partition = memory_partition(64 * 1024)
        .await
        .map_err(|error| error.to_string())?;
    let mut manager = PluginManager::open(partition)
        .await
        .map_err(|error| error.to_string())?;
    manager.install_vfs(barracuda_vfs::global_namespace().await);
    manager.install_task_spawner(spawner);
    let network = loopback_network();
    let mut context = plugin_context(&network);
    let records = Rc::new(RefCell::new(Vec::new()));
    register_services(&mut manager, &mut context);
    manager
        .register(Recorder(Rc::clone(&records)))
        .map_err(|error| error.to_string())?;
    let bench = Bench::new(network, &mut setup);
    spawner.spawn(run_bench(Rc::clone(&bench)).map_err(|error| format!("{error:?}"))?);
    manager
        .register(Probe {
            setup: Some(setup),
            bench: Rc::clone(&bench),
            scenario: Receiving,
            runtime: None,
            completed: Rc::new(Cell::new(false)),
        })
        .map_err(|error| error.to_string())?;
    manager.start().map_err(|error| error.to_string())?;
    Ok(Running {
        manager,
        records,
        bench,
    })
}

/// Hands the receive runtime to the probe's task.
struct Receiving;

impl Scenario for Receiving {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.runtime.take()
    }
}

async fn end_to_end_steps(spawner: Spawner) -> Result<(), String> {
    install_global_memory_vfs()
        .await
        .map_err(|error| error.to_string())?;
    let partition = memory_partition(64 * 1024)
        .await
        .map_err(|error| error.to_string())?;
    let mut manager = PluginManager::open(partition)
        .await
        .map_err(|error| error.to_string())?;
    manager.install_vfs(barracuda_vfs::global_namespace().await);
    manager.install_task_spawner(spawner);
    let network = loopback_network();
    let mut context = plugin_context(&network);
    let records = Rc::new(RefCell::new(Vec::new()));
    let snapshots: Snapshots = Rc::new(RefCell::new(None));
    register_services(&mut manager, &mut context);
    manager
        .register(Recorder(Rc::clone(&records)))
        .map_err(|error| error.to_string())?;
    let owner = br#"[{"id":"7","label":"Ann"}]"#.to_vec();
    let mut setup = Setup {
        stored: vec![("owners", owner)],
        receive: vec![
            ok(json!([
                text_update(10, 7, "hello"),
                text_update(11, 8, "hi"),
                text_update(12, 9, &format!("/start {PAIRING_CODE}")),
            ])),
            // The connection drops; the server then sends 12 again.
            ScriptStep::ConnectError(embedded_io_async::ErrorKind::ConnectionReset),
            ok(json!([
                text_update(12, 9, &format!("/start {PAIRING_CODE}")),
                text_update(13, 7, "again"),
            ])),
            idle(),
        ],
        send: vec![ok(json!({"message_id": 500, "chat": {"id": 9}}))],
        ..Setup::configured("send_receive")
    };
    let bench = Bench::new(network, &mut setup);
    spawner.spawn(run_bench(Rc::clone(&bench)).map_err(|error| format!("{error:?}"))?);
    manager
        .register(Probe {
            setup: Some(setup),
            bench,
            scenario: Started(Rc::clone(&snapshots)),
            runtime: None,
            completed: Rc::new(Cell::new(false)),
        })
        .map_err(|error| error.to_string())?;
    manager.start().map_err(|error| error.to_string())?;

    let (receive, send) = NETWORKS
        .with(|networks| networks.borrow().clone())
        .ok_or("networks")?;
    let finished = with_timeout(Duration::from_secs(5), async {
        while records.borrow().len() < 2 || receive.requests().len() < 4 {
            Timer::after_millis(5).await;
        }
    })
    .await;
    if finished.is_err() {
        return Err(format!(
            "timed out: records {:?}, polls {:?}",
            records.borrow(),
            targets(receive.requests())
        ));
    }
    Timer::after_millis(20).await;

    let records = records.borrow().clone();
    let snapshot = (snapshots.borrow().as_ref().ok_or("snapshot")?)();
    let check = |condition: bool, what: &str| {
        if condition {
            Ok(())
        } else {
            Err(format!(
                "{what}: records {records:?}, polls {:?}",
                targets(receive.requests())
            ))
        }
    };
    check(
        records
            == vec![
                json!({"route": {"channel": "telegram", "conversation_id": "7"}, "message_id": "100", "text": "hello"}),
                json!({"route": {"channel": "telegram", "conversation_id": "7"}, "message_id": "130", "text": "again"}),
            ],
        "owner messages are published once each",
    )?;
    let sent = send.requests();
    check(sent.len() == 1, "one paired reply")?;
    let (head, body) = sent[0].split_once("\r\n\r\n").ok_or("request body")?;
    let body: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    check(
        head.starts_with(&format!("POST /bot{TOKEN}/sendMessage ")),
        "the reply goes through sendMessage",
    )?;
    check(
        body == json!({"chat_id": 9, "text": PAIRED_REPLY}),
        "the reply confirms the pairing in the sender's chat",
    )?;
    check(
        snapshot.owners == ["7", "9"],
        "the sender with the code paired",
    )?;
    check(
        snapshot.labels == [Some("Ann".into()), Some("@user9".into())],
        "owners keep their labels",
    )?;
    check(snapshot.ignored == 1, "the stranger is ignored")?;
    check(
        snapshot.offset == Some(14),
        "the cursor follows the last update",
    )?;
    let polls = targets(receive.requests());
    check(!polls[0].contains("offset"), "the first poll has no cursor")?;
    check(
        polls[1].ends_with("&offset=13"),
        "the next poll acknowledges",
    )?;
    check(
        polls[2].ends_with("&offset=13"),
        "a reconnect keeps the cursor",
    )?;
    check(
        polls[3].ends_with("&offset=14"),
        "the duplicate moves nothing back",
    )?;
    // The executor thread outlives the test; keep its Plugins running.
    core::mem::forget(manager);
    Ok(())
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn owners_publish_codes_pair_strangers_are_ignored_and_redeliveries_dropped() {
        on_executor(|spawner| Box::pin(end_to_end_steps(spawner)));
    }
}

// --- Media ---------------------------------------------------------------

/// Larger than any `attachment-max-bytes`.
const HUGE: u64 = 1 << 40;

/// An update whose message from `sender` carries `fields`.
fn media_update(update_id: i64, sender: i64, fields: Value) -> Value {
    let mut update = json!({
        "update_id": update_id,
        "message": {
            "message_id": update_id * 10,
            "date": 1,
            "from": {"id": sender, "is_bot": false, "first_name": "User"},
            "chat": {"id": sender, "type": "private"},
        }
    });
    for (key, value) in fields.as_object().expect("fields") {
        update["message"][key] = value.clone();
    }
    update
}

fn file(path: &str) -> ScriptStep {
    ok(json!({"file_id": "id", "file_unique_id": "u", "file_path": path}))
}

/// A response head declaring `length` body bytes, followed by none.
fn declared(content_type: &str, length: u64) -> ScriptStep {
    ScriptStep::Response {
        bytes: format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
        )
        .into_bytes(),
        max_read: usize::MAX,
        pending_after: false,
    }
}

async fn media_steps(spawner: Spawner) -> Result<(), String> {
    let setup = Setup {
        stored: vec![("owners", br#"[{"id":"7","label":"Ann"}]"#.to_vec())],
        receive: vec![
            ok(json!([
                media_update(
                    20,
                    7,
                    json!({"caption": "look", "photo": [
                        {"file_id": "small", "file_unique_id": "s", "width": 90, "height": 90, "file_size": 100},
                        {"file_id": "big", "file_unique_id": "b", "width": 9000, "height": 9000, "file_size": HUGE},
                    ]})
                ),
                media_update(
                    21,
                    8,
                    json!({"caption": "hi", "photo": [{"file_id": "theirs", "file_size": 10}]})
                ),
                media_update(
                    22,
                    7,
                    json!({"document": {"file_id": "zip", "file_name": "big.zip",
                    "mime_type": "application/zip", "file_size": HUGE}})
                ),
                media_update(
                    23,
                    7,
                    json!({"voice": {"file_id": "voice", "duration": 2,
                    "mime_type": "audio/ogg", "file_size": 10}})
                ),
                media_update(
                    24,
                    7,
                    json!({"caption": "scan", "document": {"file_id": "png",
                    "file_name": "scan.png", "mime_type": "image/png"}})
                ),
                media_update(
                    25,
                    7,
                    json!({"sticker": {"file_id": "sticker", "emoji": "👍"}})
                ),
            ])),
            idle(),
        ],
        files: vec![
            file("photos/file_1.jpg"),
            file("voice/file_2.oga"),
            file("documents/file_3.png"),
        ],
        downloads: vec![
            ScriptStep::response(200, "image/jpeg", b"JPEGDATA", usize::MAX),
            ScriptStep::json(
                404,
                r#"{"ok":false,"error_code":404,"description":"Not Found"}"#,
            ),
            declared("image/png", HUGE),
        ],
        ..Setup::configured("send_receive")
    };
    let running = start_receiving(spawner, setup).await?;
    let records = &running.records;
    let bench = &running.bench;
    let finished = with_timeout(Duration::from_secs(5), async {
        while records.borrow().len() < 4 || bench.polls.requests().len() < 2 {
            Timer::after_millis(5).await;
        }
    })
    .await;
    Timer::after_millis(20).await;
    let records = records.borrow().clone();
    let files = targets(bench.files.requests());
    let downloads = targets(bench.downloads.requests());
    let check = |condition: bool, what: &str| {
        if condition {
            Ok(())
        } else {
            Err(format!(
                "{what}: records {records:?}, getFile {files:?}, downloads {downloads:?}"
            ))
        }
    };
    check(finished.is_ok(), "timed out")?;
    check(records.len() == 4, "four owner messages are published")?;

    let photo = &records[0];
    check(
        photo["message_id"] == "200" && photo["text"] == "look",
        "the caption is the photo's text",
    )?;
    let attachment = &photo["attachments"][0];
    check(
        photo["attachments"].as_array().map(Vec::len) == Some(1)
            && attachment["kind"] == "image"
            && attachment["mime_type"] == "image/jpeg"
            && attachment["size"] == 8
            && attachment.get("filename").is_none(),
        "the photo is stored as an image",
    )?;
    let path = attachment["path"].as_str().unwrap_or_default();
    check(
        path.starts_with("/workspace/cache/gateway/telegram/") && path.ends_with(".jpg"),
        "the photo lives in the channel's attachment directory",
    )?;
    check(
        records[1]
            == json!({"route": {"channel": "telegram", "conversation_id": "7"}, "message_id": "220",
                      "text": "[file too large, not received]"}),
        "a file over the limit is a note, fetched never",
    )?;
    check(
        records[2]
            == json!({"route": {"channel": "telegram", "conversation_id": "7"}, "message_id": "230",
                      "text": "[audio not received]"}),
        "a failed download is a note",
    )?;
    check(
        records[3]
            == json!({"route": {"channel": "telegram", "conversation_id": "7"}, "message_id": "240",
                      "text": "scan\n[image too large, not received]"}),
        "a download declaring too many bytes is a note under the caption",
    )?;
    // The smallest photo size fits; the stranger's photo and the
    // oversized document are never asked for.
    check(
        files
            == [
                format!("/bot{TOKEN}/getFile?file_id=small"),
                format!("/bot{TOKEN}/getFile?file_id=voice"),
                format!("/bot{TOKEN}/getFile?file_id=png"),
            ],
        "getFile only for the owner's files within the limit",
    )?;
    check(
        downloads
            == [
                format!("/file/bot{TOKEN}/photos/file_1.jpg"),
                format!("/file/bot{TOKEN}/voice/file_2.oga"),
                format!("/file/bot{TOKEN}/documents/file_3.png"),
            ],
        "each file is downloaded from its getFile path",
    )?;
    core::mem::forget(running.manager);
    Ok(())
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn owners_media_is_stored_and_a_file_that_does_not_arrive_becomes_a_note() {
        on_executor(|spawner| Box::pin(media_steps(spawner)));
    }
}

/// Serves one in-memory connection through the WebServer's routes.
mod routes {
    use alloc::vec::Vec;
    use core::future::Future;

    use barracuda_webserver_plugin::WebServer;
    use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
    use picoserve::io::Socket;
    use picoserve::time::{Duration, TimeoutError, Timer};

    /// Returns the WebServer's raw answer to the raw request `input`.
    pub(super) async fn send(server: &WebServer, input: Vec<u8>) -> Vec<u8> {
        let mut output = Output(Vec::new());
        server
            .serve_connection(
                TestTimer,
                &mut [0; 4096],
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

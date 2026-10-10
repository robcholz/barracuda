//! The Inkbox receive loop against a scripted iMessage API, real Plugin
//! storage, and the real Gateway and Workflow.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs
)]

extern crate std;

use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use std::sync::mpsc::{sync_channel, SyncSender};

use barracuda_imessage_gateway_channel::{
    status_response, ModeEndpoint, ReceiveState, MODE_STORAGE_KEY, PAIRED_REPLY,
};
use barracuda_imessage_gateway_plugin::IMessageGatewayPlugin;
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_platform_test::{
    install_global_memory_vfs, memory_partition, never_embassy_stack, ScriptStep, ScriptedStack,
};
use barracuda_plugin::manager::{PluginDeclaration, PluginManager};
use barracuda_workflow_plugin::{
    workflow_action_schema_inline, WorkflowActionFuture, WorkflowActionHandler,
    WorkflowActionRegistry, WorkflowActionSchema, WorkflowPlugin, WorkflowService,
};
use embassy_executor::{Executor, Spawner};
use embassy_futures::select::{select, Either};
use embassy_time::{with_timeout, Timer};
use futures_lite::future::block_on;
use http_client::ReceiveLease;
use serde_json::{json, Value};

use super::*;
use crate::receive::CURSOR_STORAGE_KEY;

type Scripted = ScriptedStack;
type State<Storage> = InkboxState<Storage, Scripted, Scripted, Scripted>;

const CONFIG: &str =
    r#"{"api_key":"key-1","identity_id":"identity-1","api_base":"http://inkbox.test"}"#;
const PAIRING_CODE: &str = "123456";
const OWNER: &str = "+15550000001";
const TIMING: ReceiveTiming = ReceiveTiming {
    initial_backoff: Duration::from_millis(20),
    max_backoff: Duration::from_millis(80),
    slot_retry: Duration::from_millis(20),
};
const POLL: Duration = Duration::from_millis(10);

/// Entropy that counts up from a seed, so the first pairing code is the seed.
#[derive(Clone)]
struct Sequence;

std::thread_local! {
    static SEQUENCE: Cell<u32> = const { Cell::new(0) };
}

impl Entropy for Sequence {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        let value = SEQUENCE.with(|sequence| {
            let value = sequence.get();
            sequence.set(value.wrapping_add(1));
            value
        });
        for chunk in bytes.chunks_mut(4) {
            chunk.copy_from_slice(&value.to_le_bytes()[..chunk.len()]);
        }
        Ok(())
    }
}

fn pairing_entropy() -> PairingEntropy {
    SEQUENCE.with(|sequence| sequence.set(123_456));
    PairingEntropy::new(Sequence)
}

fn message(id: &str, direction: &str, from: &str, text: Option<&str>, at: &str) -> Value {
    json!({
        "id": id,
        "conversation_id": format!("conv-{from}"),
        "reply_to_message_id": null,
        "thread_id": "thread",
        "direction": direction,
        "remote_number": from,
        "sender_number": null,
        "is_group": false,
        "content": text,
        "message_type": "message",
        "service": "imessage",
        "media": null,
        "status": "received",
        "is_read": false,
        "reactions": null,
        "created_at": at,
        "updated_at": at,
    })
}

fn list(messages: &[Value]) -> ScriptStep {
    ScriptStep::json(200, &Value::Array(messages.to_vec()).to_string())
}

/// A response with extra headers, kept alive.
fn raw(status: u16, headers: &str, body: &str) -> ScriptStep {
    let bytes = format!(
        "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}Connection: keep-alive\r\n\r\n{body}",
        body.len()
    )
    .into_bytes();
    ScriptStep::Response {
        bytes,
        max_read: usize::MAX,
        pending_after: false,
    }
}

/// A poll the server never answers.
fn idle() -> ScriptStep {
    ScriptStep::pending_after_headers(200, "application/json")
}

fn targets(network: &ScriptedStack) -> Vec<String> {
    network
        .requests()
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

fn leak(steps: Vec<ScriptStep>) -> &'static ScriptedStack {
    Box::leak(Box::new(ScriptedStack::new(steps)))
}

struct Setup {
    configured: bool,
    mode: Option<&'static str>,
    stored: Vec<(&'static str, Vec<u8>)>,
    receive: Vec<ScriptStep>,
    send: Vec<ScriptStep>,
    hold_slot: bool,
}

impl Setup {
    fn configured(mode: &'static str) -> Self {
        Self {
            configured: true,
            mode: Some(mode),
            stored: Vec::new(),
            receive: Vec::new(),
            send: Vec::new(),
            hold_slot: false,
        }
    }
}

struct Harness<Storage: PluginStorage> {
    state: Rc<State<Storage>>,
    runtime: RefCell<Option<ReceiveRuntime>>,
    receive: &'static ScriptedStack,
    send: &'static ScriptedStack,
    held: RefCell<Option<ReceiveLease<Scripted, Scripted>>>,
    storage: Storage,
}

impl<Storage: PluginStorage> Harness<Storage> {
    fn run_until(&self, what: &str, done: impl Fn() -> bool) {
        let mut runtime = self.runtime.borrow_mut();
        let runtime = runtime.as_mut().expect("receive runtime");
        let waited = block_on(select(
            runtime,
            with_timeout(Duration::from_secs(5), async {
                while !done() {
                    Timer::after_millis(2).await;
                }
            }),
        ));
        assert!(
            matches!(waited, Either::Second(Ok(()))),
            "timed out waiting for {what}: state {:?}, polls {:?}",
            self.state(),
            targets(self.receive)
        );
    }

    fn run_for(&self, duration: Duration) {
        let mut runtime = self.runtime.borrow_mut();
        let runtime = runtime.as_mut().expect("receive runtime");
        let _elapsed = block_on(select(runtime, Timer::after(duration)));
    }

    fn state(&self) -> ReceiveState {
        ChannelControl::receive(&*self.state).state()
    }

    fn polls(&self) -> Vec<String> {
        targets(self.receive)
    }

    fn set_mode(&self, mode: &str) -> HttpResponse {
        let endpoint = ModeEndpoint::new(Rc::clone(&self.state));
        let body = format!(r#"{{"mode":"{mode}"}}"#);
        block_on(endpoint.handle(HttpRequest::new(HttpMethod::Post, body.into_bytes())))
    }

    fn status(&self) -> Value {
        let response = status_response(&*self.state);
        serde_json::from_slice(response.body().expect("buffered")).expect("JSON")
    }

    fn stored(&self, key: &str) -> Option<Value> {
        block_on(self.storage.get_bytes(key))
            .expect("read storage")
            .map(|bytes| serde_json::from_slice(&bytes).expect("stored JSON"))
    }
}

trait Scenario: 'static {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime>;
}

struct Probe<S> {
    setup: Option<Setup>,
    scenario: S,
    runtime: Option<ReceiveRuntime>,
    completed: Rc<Cell<bool>>,
}

impl<S> PluginDeclaration for Probe<S> {
    const ID: &'static str = "inkbox-receive-probe";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway"];
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
            block_on(storage.put(MODE_STORAGE_KEY, format!("\"{mode}\"").as_bytes()))?;
        }
        for (key, value) in &setup.stored {
            block_on(storage.put(key, value.as_slice()))?;
        }
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let receive = leak(setup.receive);
        let send = leak(setup.send);
        let slots = ReceiveSlots::from_connectors([receive.clone()], receive);
        let held = setup
            .hold_slot
            .then(|| slots.acquire().expect("a free slot"));
        let (state, runtime) = build_state(
            storage.clone(),
            gateway,
            ClientFactory::from_network(send, send),
            InkboxSlots(slots),
            pairing_entropy(),
            "http://inkbox.test".into(),
            Timing {
                receive: TIMING,
                poll: POLL,
            },
        )?;
        self.runtime = self.scenario.run(Harness {
            state,
            runtime: RefCell::new(Some(runtime)),
            receive,
            send,
            held: RefCell::new(held),
            storage,
        });
        self.completed.set(true);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        if let Some(runtime) = self.runtime.take() {
            let task = inkbox_receive_task(runtime, context.task_token())
                .map_err(PluginError::registration)?;
            context.task_spawner()?.spawn(task);
        }
        Ok(())
    }
}

fn plugin_context() -> PluginContext {
    let stack = never_embassy_stack();
    let info = barracuda_plugin::api::TargetIdentity::new(
        barracuda_plugin::api::PlatformInfo::new("test", "test", "test-arch", "hosted"),
        barracuda_plugin::api::BoardInfo::new(
            "test-board",
            barracuda_plugin::api::Hardware::new("test-chip"),
        ),
    );
    PluginContext::new(info, stack, ClientFactory::plaintext(stack))
}

fn run(setup: Setup, scenario: impl Scenario) {
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("create test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let mut context = plugin_context();
    let completed = Rc::new(Cell::new(false));
    manager
        .register(WorkflowPlugin::new(&mut context))
        .expect("register Workflow Plugin");
    manager
        .register(IMessageGatewayPlugin::new(&mut context))
        .expect("register IMessage Gateway Plugin");
    manager
        .register(Probe {
            setup: Some(setup),
            scenario,
            runtime: None,
            completed: Rc::clone(&completed),
        })
        .expect("run the Inkbox scenario");
    assert!(completed.get());
}

// --- Polling, cursor, and errors -------------------------------------------

struct FirstPolls;

impl Scenario for FirstPolls {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("three polls", || harness.polls().len() >= 3);
        let polls = harness.polls();
        // The first poll only finds where the identity's messages end.
        assert_eq!(polls[0], "/api/v1/imessage/messages?limit=1");
        assert_eq!(
            polls[1],
            "/api/v1/imessage/messages?limit=50&start_datetime=2026-10-09T01%3A00%3A00.123456%2B00%3A00"
        );
        let requests = harness.receive.requests();
        assert!(requests[0].contains("X-API-Key: key-1\r\n"));
        // One kept-alive connection serves every poll.
        assert_eq!(harness.receive.connect_count(), 1);
        assert_eq!(harness.state(), ReceiveState::Receiving);
        assert_eq!(
            harness.status()["receive"],
            json!({"state": "receiving", "slots": {"in_use": 1, "capacity": 1}})
        );
        // The baseline cursor is stored at once.
        assert_eq!(
            harness.stored(CURSOR_STORAGE_KEY),
            Some(json!({"start": "2026-10-09T01:00:00.123456+00:00", "recent": ["old"]}))
        );
        // The old message was never handled: nobody asked to pair with it.
        assert!(harness.send.requests().is_empty());
        None
    }
}

#[test]
fn the_first_poll_starts_after_the_newest_message_on_one_connection() {
    run(
        Setup {
            receive: vec![
                list(&[message(
                    "old",
                    "inbound",
                    "+1555",
                    Some(PAIRING_CODE),
                    "2026-10-09T01:00:00.123456+00:00",
                )]),
                list(&[message(
                    "old",
                    "inbound",
                    "+1555",
                    Some(PAIRING_CODE),
                    "2026-10-09T01:00:00.123456+00:00",
                )]),
                idle(),
            ],
            ..Setup::configured("send_receive")
        },
        FirstPolls,
    );
}

struct EmptyInbox;

impl Scenario for EmptyInbox {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("a second poll", || harness.polls().len() >= 2);
        assert_eq!(
            harness.polls()[1],
            "/api/v1/imessage/messages?limit=50&start_datetime=1970-01-01T00%3A00%3A00Z"
        );
        None
    }
}

#[test]
fn an_empty_inbox_starts_from_the_epoch() {
    run(
        Setup {
            receive: vec![list(&[]), idle()],
            ..Setup::configured("send_receive")
        },
        EmptyInbox,
    );
}

struct RestoredCursor;

impl Scenario for RestoredCursor {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("two polls", || harness.polls().len() >= 2);
        assert!(harness.polls()[0].ends_with("&start_datetime=2026-10-09T01%3A02%3A03Z"));
        // "seen" is in the restored ring and is not handled again; the
        // outbound message moves the cursor but is not handled.
        harness.run_until("the cursor", || {
            harness
                .stored(CURSOR_STORAGE_KEY)
                .is_some_and(|cursor| cursor["start"] == "2026-10-09T01:05:00Z")
        });
        assert!(harness.send.requests().is_empty());
        let cursor = harness.stored(CURSOR_STORAGE_KEY).unwrap();
        assert_eq!(cursor["recent"], json!(["seen", "mine"]));
        None
    }
}

#[test]
fn the_cursor_is_restored_and_handled_ids_are_dropped() {
    run(
        Setup {
            stored: vec![(
                CURSOR_STORAGE_KEY,
                br#"{"start":"2026-10-09T01:02:03Z","recent":["seen"]}"#.to_vec(),
            )],
            receive: vec![
                list(&[
                    message(
                        "mine",
                        "outbound",
                        "+1555",
                        Some("sent"),
                        "2026-10-09T01:05:00Z",
                    ),
                    message(
                        "seen",
                        "inbound",
                        "+1555",
                        Some(PAIRING_CODE),
                        "2026-10-09T01:02:03Z",
                    ),
                ]),
                idle(),
            ],
            ..Setup::configured("send_receive")
        },
        RestoredCursor,
    );
}

struct RateLimited;

impl Scenario for RateLimited {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("the rate limit", || {
            matches!(harness.state(), ReceiveState::Error(_))
        });
        assert_eq!(
            harness.state(),
            ReceiveState::Error("Inkbox rate limit".into())
        );
        // Retry-After: 7 holds the next poll back far longer than the backoff.
        harness.run_for(Duration::from_millis(200));
        assert_eq!(harness.polls().len(), 1);
        None
    }
}

#[test]
fn a_rate_limit_waits_for_retry_after() {
    run(
        Setup {
            stored: vec![(
                CURSOR_STORAGE_KEY,
                br#"{"start":"2026-10-09T01:02:03Z"}"#.to_vec(),
            )],
            receive: vec![
                raw(429, "Retry-After: 7\r\n", r#"{"detail":"slow down"}"#),
                idle(),
            ],
            ..Setup::configured("send_receive")
        },
        RateLimited,
    );
}

struct BadKey;

impl Scenario for BadKey {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("halted", || {
            matches!(harness.state(), ReceiveState::Error(_))
        });
        harness.run_for(Duration::from_millis(100));
        assert_eq!(harness.polls().len(), 1);
        assert!(harness
            .state()
            .message()
            .unwrap()
            .contains("Inkbox rejected the API key"));
        assert_eq!(harness.status()["receive"]["state"], "error");
        None
    }
}

#[test]
fn a_rejected_api_key_halts_receiving() {
    run(
        Setup {
            receive: vec![ScriptStep::json(401, r#"{"detail":"Invalid API key"}"#)],
            ..Setup::configured("send_receive")
        },
        BadKey,
    );
}

struct LargePage;

impl Scenario for LargePage {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("a smaller page", || harness.polls().len() >= 2);
        assert!(harness.polls()[0].contains("limit=50&"));
        assert!(harness.polls()[1].contains("limit=25&"));
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        None
    }
}

#[test]
fn a_page_over_the_body_limit_is_asked_for_again_smaller() {
    let long = "x".repeat(crate::receive::MAX_BODY_BYTES);
    run(
        Setup {
            stored: vec![(
                CURSOR_STORAGE_KEY,
                br#"{"start":"2026-10-09T01:02:03Z"}"#.to_vec(),
            )],
            receive: vec![
                list(&[message(
                    "big",
                    "inbound",
                    "+1555",
                    Some(&long),
                    "2026-10-09T01:05:00Z",
                )]),
                list(&[]),
                idle(),
            ],
            ..Setup::configured("send_receive")
        },
        LargePage,
    );
}

// --- Modes and slots -------------------------------------------------------

struct Modes;

impl Scenario for Modes {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        assert_eq!(harness.set_mode("send").status(), 204);
        harness.run_until("idle", || harness.state() == ReceiveState::Idle);
        assert_eq!(ChannelControl::receive(&*harness.state).in_use(), 0);
        let polls = harness.polls().len();
        harness.run_for(Duration::from_millis(60));
        assert_eq!(harness.polls().len(), polls);
        assert_eq!(
            harness.status(),
            json!({"configured": true, "mode": "send", "owners": {"count": 0}})
        );
        assert_eq!(harness.set_mode("send_receive").status(), 204);
        harness.run_until("polling again", || harness.polls().len() > polls);
        None
    }
}

#[test]
fn modes_stop_and_restart_polling() {
    run(
        Setup {
            stored: vec![(
                CURSOR_STORAGE_KEY,
                br#"{"start":"2026-10-09T01:02:03Z"}"#.to_vec(),
            )],
            receive: vec![list(&[]), list(&[]), idle(), list(&[]), idle()],
            ..Setup::configured("send_receive")
        },
        Modes,
    );
}

struct Legacy;

impl Scenario for Legacy {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        assert_eq!(harness.state.mode(), ChannelMode::Send);
        harness.run_for(Duration::from_millis(50));
        assert!(harness.polls().is_empty());
        assert_eq!(harness.stored(MODE_STORAGE_KEY), Some(json!("send")));
        None
    }
}

#[test]
fn a_configuration_stored_before_modes_sends_only() {
    run(
        Setup {
            mode: None,
            ..Setup::configured("send")
        },
        Legacy,
    );
}

struct FullPool;

impl Scenario for FullPool {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        assert_eq!(harness.state(), ReceiveState::NoSlot);
        harness.run_for(Duration::from_millis(60));
        assert!(harness.polls().is_empty());
        harness.held.borrow_mut().take();
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        None
    }
}

#[test]
fn a_full_pool_waits_for_a_slot() {
    run(
        Setup {
            hold_slot: true,
            receive: vec![list(&[]), idle()],
            ..Setup::configured("send_receive")
        },
        FullPool,
    );
}

// --- Owners, publishing, and dedup with Workflow running --------------------

struct Record(Rc<RefCell<Vec<Value>>>);

impl WorkflowActionHandler for Record {
    type Request = Value;
    type Response = Value;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
        "test.record",
        r#"{"type":"object","properties":{"route":{"type":"object"},"message_id":{"type":"string"},"text":{"type":"string"}}}"#,
        r#"{"type":"object"}"#
    );

    fn invoke(&self, request: Value) -> WorkflowActionFuture<'_, Value> {
        self.0.borrow_mut().push(request);
        Box::pin(async { Ok(json!({})) })
    }
}

const RECORD_WORKFLOW: &str = r#"{
    "id": "record-inbound",
    "match": {"event": "gateway.message.received"},
    "steps": [{"call": "test.record", "arguments": {
        "route": "$event.input.route",
        "message_id": "$event.input.message_id",
        "text": "$event.input.text"
    }}]
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

struct Snapshot {
    owners: Vec<String>,
    ignored: u32,
}

type Snapshots = Rc<RefCell<Option<Box<dyn Fn() -> Snapshot>>>>;

struct Started(Snapshots);

std::thread_local! {
    static NETWORKS: RefCell<Option<(&'static ScriptedStack, &'static ScriptedStack)>> =
        const { RefCell::new(None) };
}

impl Scenario for Started {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        let pairing = harness
            .state
            .owners()
            .expect("owners")
            .pairing()
            .expect("pairing code");
        assert_eq!(pairing.code.as_str(), PAIRING_CODE);
        let state = Rc::clone(&harness.state);
        self.0.replace(Some(Box::new(move || Snapshot {
            owners: state
                .owners()
                .expect("owners")
                .owners()
                .iter()
                .map(|owner| owner.id().into())
                .collect(),
            ignored: state.owners().expect("owners").ignored(),
        })));
        NETWORKS.with(|networks| networks.replace(Some((harness.receive, harness.send))));
        harness.runtime.take()
    }
}

#[embassy_executor::task]
async fn end_to_end(spawner: Spawner, done: SyncSender<Result<(), String>>) {
    let outcome = end_to_end_steps(spawner).await;
    let _sent = done.send(outcome);
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
    let mut context = plugin_context();
    let records = Rc::new(RefCell::new(Vec::new()));
    let snapshots: Snapshots = Rc::new(RefCell::new(None));
    manager
        .register(WorkflowPlugin::new(&mut context))
        .map_err(|error| error.to_string())?;
    manager
        .register(IMessageGatewayPlugin::new(&mut context))
        .map_err(|error| error.to_string())?;
    manager
        .register(Recorder(Rc::clone(&records)))
        .map_err(|error| error.to_string())?;
    let owner = format!(r#"[{{"id":"{OWNER}"}}]"#).into_bytes();
    let at = |second: u32| format!("2026-10-09T01:00:{second:02}Z");
    let hello = message("m1", "inbound", OWNER, Some(" hello "), &at(1));
    let stranger = message("m2", "inbound", "+1666", Some("hi"), &at(2));
    let code = message("m3", "inbound", "+1777", Some(PAIRING_CODE), &at(3));
    let photo = message("m4", "inbound", OWNER, None, &at(4));
    let again = message("m5", "inbound", OWNER, Some("again"), &at(5));
    manager
        .register(Probe {
            setup: Some(Setup {
                stored: vec![
                    ("owners", owner),
                    (
                        CURSOR_STORAGE_KEY,
                        br#"{"start":"2026-10-09T01:00:00Z"}"#.to_vec(),
                    ),
                ],
                receive: vec![
                    list(&[photo.clone(), code.clone(), stranger.clone(), hello.clone()]),
                    // The connection drops; the next poll repeats the newest.
                    ScriptStep::ConnectError(embedded_io_async::ErrorKind::ConnectionReset),
                    list(&[again.clone(), photo.clone()]),
                    idle(),
                ],
                send: vec![ScriptStep::json(201, r#"{"message":{"id":"reply-1"}}"#)],
                ..Setup::configured("send_receive")
            }),
            scenario: Started(Rc::clone(&snapshots)),
            runtime: None,
            completed: Rc::new(Cell::new(false)),
        })
        .map_err(|error| error.to_string())?;
    manager.start().map_err(|error| error.to_string())?;

    let (receive, send) = NETWORKS
        .with(|networks| *networks.borrow())
        .ok_or("networks")?;
    let finished = with_timeout(Duration::from_secs(5), async {
        while records.borrow().len() < 2 || receive.requests().len() < 3 {
            Timer::after_millis(5).await;
        }
    })
    .await;
    if finished.is_err() {
        return Err(format!(
            "timed out: records {:?}, polls {:?}",
            records.borrow(),
            targets(receive)
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
                "{what}: records {records:?}, polls {:?}, sent {:?}",
                targets(receive),
                send.requests()
            ))
        }
    };
    check(
        records
            == vec![
                json!({"route": {"channel": "inkbox", "conversation_id": format!("conv-{OWNER}")}, "message_id": "m1", "text": "hello"}),
                json!({"route": {"channel": "inkbox", "conversation_id": format!("conv-{OWNER}")}, "message_id": "m5", "text": "again"}),
            ],
        "owner messages are published once each, oldest first",
    )?;
    check(
        targets(receive)[1].ends_with("&start_datetime=2026-10-09T01%3A00%3A04Z"),
        "the cursor follows the newest message",
    )?;
    let sent = send.requests();
    check(sent.len() == 1, "one paired reply")?;
    let (head, body) = sent[0].split_once("\r\n\r\n").ok_or("request body")?;
    let body: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    check(
        head.starts_with("POST /api/v1/imessage/messages?agent_identity_id=identity-1 "),
        "the reply goes through the send path",
    )?;
    check(
        body == json!({"conversation_id": "conv-+1777", "text": PAIRED_REPLY}),
        "the reply confirms the pairing in the sender's conversation",
    )?;
    check(
        snapshot.owners == [OWNER, "+1777"],
        "the sender with the code paired",
    )?;
    check(snapshot.ignored == 1, "the stranger is counted")?;
    Ok(())
}

#[test]
fn owners_publish_codes_pair_strangers_are_ignored_and_repeats_dropped() {
    let (done, outcome) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner.spawn(end_to_end(spawner, done).expect("spawn the end-to-end test"));
        });
    });
    let outcome = outcome
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the end-to-end test finished");
    if let Err(message) = outcome {
        panic!("{message}");
    }
}

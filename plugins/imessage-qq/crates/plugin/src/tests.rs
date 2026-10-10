//! The QQ channel against a scripted OpenAPI, an in-memory WebSocket gateway,
//! real Plugin storage, and the real Gateway and Workflow.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    missing_docs
)]

extern crate std;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use core::cell::{Cell, RefCell};
use core::task::{Poll, Waker};
use std::sync::mpsc::{sync_channel, SyncSender};

use barracuda_captive_portal_plugin::{EntryState, EntryStatus};
use barracuda_imessage_gateway_channel::{
    entry_status, ChannelControl, ChannelEndpoint, ModeEndpoint, ReceiveChannel, ReceiveError,
    ReceiveFuture, ReceiveSession, ReceiveSlotSource, ReceiveState, MODE_STORAGE_KEY, PAIRED_REPLY,
};
use barracuda_imessage_gateway_plugin::{
    ChannelError, ChannelFuture, IMessageGatewayPlugin, MessageChannel, Operation,
    SendMessageRequest, SendReceipt,
};
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
use embassy_time::{with_timeout, Duration, Timer};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use futures_lite::future::block_on;
use serde_json::{json, Value};

use super::*;
use crate::receive::SESSION_STORAGE_KEY;

type Scripted = ScriptedStack;
type Channel<Storage> = QQChannel<Storage, TestSlots, Scripted, Scripted>;

const CONFIG: &str = r#"{"app_id":"app","app_secret":"secret","api_base":"http://qq.test","token_url":"http://qq.test/app/getAppAccessToken"}"#;
const TOKEN: &str = r#"{"access_token":"token-1","expires_in":"7200"}"#;
const GATEWAY: &str = r#"{"url":"wss://gateway.qq.test/websocket/"}"#;
const MESSAGE_SENT: &str = r#"{"id":"reply-1"}"#;
const REJECTED: &str = r#"{"code":10004,"message":"机器人不存在"}"#;
const PAIRING_CODE: &str = "123456";
const TIMING: ReceiveTiming = ReceiveTiming {
    initial_backoff: Duration::from_millis(20),
    max_backoff: Duration::from_millis(80),
    slot_retry: Duration::from_millis(20),
};

// --- Entropy ---------------------------------------------------------------

/// Entropy that fills every four bytes with a per-thread counter, starting
/// at a seed, so the first pairing code is the seed.
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

fn entropy() -> SharedEntropy {
    SEQUENCE.with(|sequence| sequence.set(123_456));
    SharedEntropy::new(Sequence)
}

// --- In-memory WebSocket gateway -------------------------------------------

/// What the fake gateway answers on its own.
#[derive(Clone, Copy)]
struct Behaviour {
    /// Answer the upgrade with 403 instead of 101.
    refuse_upgrade: bool,
    hello_interval_ms: u64,
    /// READY after Identify, RESUMED after Resume.
    login: bool,
    /// Acknowledge heartbeats.
    ack: bool,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            refuse_upgrade: false,
            hello_interval_ms: 45_000,
            login: true,
            ack: true,
        }
    }
}

/// One accepted connection: bytes for the client, and the client's frames.
#[derive(Default)]
struct Connection {
    to_client: RefCell<VecDeque<u8>>,
    closed: Cell<bool>,
    waker: RefCell<Option<Waker>>,
    from_client: RefCell<Vec<u8>>,
    upgraded: Cell<bool>,
}

impl Connection {
    fn push(&self, bytes: &[u8]) {
        self.to_client.borrow_mut().extend(bytes);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }

    fn end(&self) {
        self.closed.set(true);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

/// An unmasked server frame.
fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x80 | opcode];
    if payload.len() < 126 {
        bytes.push(payload.len() as u8);
    } else {
        bytes.push(126);
        bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    bytes.extend_from_slice(payload);
    bytes
}

fn text_frame(value: &Value) -> Vec<u8> {
    frame(0x1, value.to_string().as_bytes())
}

fn close_frame(code: u16) -> Vec<u8> {
    frame(0x8, &code.to_be_bytes())
}

/// The fake QQ gateway: accepts connections and records what clients send.
#[derive(Default)]
struct FakeGateway {
    behaviour: RefCell<VecDeque<Behaviour>>,
    current: RefCell<Option<(Rc<Connection>, Behaviour)>>,
    connections: Cell<usize>,
    /// Every text frame clients sent, in order, across connections.
    texts: RefCell<Vec<Value>>,
    /// Close codes clients sent.
    client_closes: RefCell<Vec<u16>>,
    seq: Cell<u64>,
}

impl FakeGateway {
    /// Behaviour of the next connections, in order; later ones use the default.
    fn script(&self, behaviours: impl IntoIterator<Item = Behaviour>) {
        self.behaviour.borrow_mut().extend(behaviours);
    }

    fn accept(self: &Rc<Self>) -> Rc<Connection> {
        let connection = Rc::new(Connection::default());
        let behaviour = self.behaviour.borrow_mut().pop_front().unwrap_or_default();
        self.connections.set(self.connections.get() + 1);
        if let Some((previous, _)) = self
            .current
            .replace(Some((Rc::clone(&connection), behaviour)))
        {
            previous.end();
        }
        connection
    }

    fn connection(&self) -> Rc<Connection> {
        Rc::clone(&self.current.borrow().as_ref().expect("a connection").0)
    }

    fn next_seq(&self) -> u64 {
        self.seq.set(self.seq.get() + 1);
        self.seq.get()
    }

    /// Sends a dispatch on the current connection.
    fn dispatch(&self, event: &str, data: Value) -> u64 {
        let seq = self.next_seq();
        self.connection().push(&text_frame(
            &json!({"op": 0, "s": seq, "t": event, "d": data}),
        ));
        seq
    }

    fn close(&self, code: u16) {
        self.connection().push(&close_frame(code));
    }

    fn ops(&self) -> Vec<u64> {
        self.texts
            .borrow()
            .iter()
            .map(|text| text["op"].as_u64().unwrap_or(99))
            .collect()
    }

    /// Handles bytes a client wrote to `connection`.
    fn received(&self, connection: &Connection, behaviour: Behaviour) {
        if !connection.upgraded.get() {
            let request = connection.from_client.borrow().clone();
            let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
                return;
            };
            connection.from_client.borrow_mut().drain(..end + 4);
            let head = String::from_utf8(request[..end].to_vec()).expect("UTF-8 request");
            assert!(head.starts_with("GET /websocket/ HTTP/1.1\r\nHost: gateway.qq.test\r\n"));
            let key = head
                .lines()
                .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
                .expect("a key");
            connection.upgraded.set(true);
            if behaviour.refuse_upgrade {
                connection.push(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
                return;
            }
            let accept = ws_client::accept_key(key.as_bytes());
            let mut response = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                core::str::from_utf8(&accept).unwrap()
            )
            .into_bytes();
            response.extend(text_frame(
                &json!({"op": 10, "d": {"heartbeat_interval": behaviour.hello_interval_ms}}),
            ));
            connection.push(&response);
        }
        loop {
            let mut bytes = connection.from_client.borrow_mut();
            if bytes.len() < 2 {
                return;
            }
            assert_eq!(bytes[1] & 0x80, 0x80, "client frames are masked");
            let (header, length) = match bytes[1] & 0x7F {
                126 => (4, usize::from(u16::from_be_bytes([bytes[2], bytes[3]]))),
                127 => panic!("unexpectedly large client frame"),
                short => (2, usize::from(short)),
            };
            if bytes.len() < header + 4 + length {
                return;
            }
            let opcode = bytes[0] & 0x0F;
            let mask: [u8; 4] = bytes[header..header + 4].try_into().unwrap();
            let payload: Vec<u8> = bytes[header + 4..header + 4 + length]
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4])
                .collect();
            bytes.drain(..header + 4 + length);
            drop(bytes);
            match opcode {
                0x1 => {
                    let text: Value = serde_json::from_slice(&payload).expect("JSON text");
                    let op = text["op"].as_u64();
                    self.texts.borrow_mut().push(text);
                    match op {
                        Some(2) if behaviour.login => {
                            let seq = self.next_seq();
                            connection.push(&text_frame(&json!({
                                "op": 0, "s": seq, "t": "READY",
                                "d": {"version": 1, "session_id": "session-1", "user": {"id": "bot"}, "shard": [0, 0]}
                            })));
                        }
                        Some(6) if behaviour.login => {
                            let seq = self.next_seq();
                            connection.push(&text_frame(
                                &json!({"op": 0, "s": seq, "t": "RESUMED", "d": ""}),
                            ));
                        }
                        Some(1) if behaviour.ack => {
                            connection.push(&text_frame(&json!({"op": 11})))
                        }
                        _ => {}
                    }
                }
                0x8 => {
                    if payload.len() >= 2 {
                        self.client_closes
                            .borrow_mut()
                            .push(u16::from_be_bytes([payload[0], payload[1]]));
                    }
                }
                0xA => {}
                other => panic!("unexpected client opcode {other}"),
            }
        }
    }
}

#[derive(Debug)]
struct PipeError;

impl core::fmt::Display for PipeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("pipe closed")
    }
}

impl core::error::Error for PipeError {}

impl embedded_io_async::Error for PipeError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::ConnectionReset
    }
}

struct ConnectionReader(Rc<Connection>);

impl ErrorType for ConnectionReader {
    type Error = PipeError;
}

impl Read for ConnectionReader {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, PipeError> {
        core::future::poll_fn(|context| {
            let mut bytes = self.0.to_client.borrow_mut();
            if bytes.is_empty() {
                if self.0.closed.get() {
                    return Poll::Ready(Ok(0));
                }
                self.0.waker.replace(Some(context.waker().clone()));
                return Poll::Pending;
            }
            let count = buffer.len().min(bytes.len());
            for slot in buffer.iter_mut().take(count) {
                *slot = bytes.pop_front().unwrap();
            }
            Poll::Ready(Ok(count))
        })
        .await
    }
}

struct ConnectionWriter {
    connection: Rc<Connection>,
    gateway: Rc<FakeGateway>,
    behaviour: Behaviour,
}

impl ErrorType for ConnectionWriter {
    type Error = PipeError;
}

impl Write for ConnectionWriter {
    async fn write(&mut self, bytes: &[u8]) -> Result<usize, PipeError> {
        if self.connection.closed.get() {
            return Err(PipeError);
        }
        self.connection
            .from_client
            .borrow_mut()
            .extend_from_slice(bytes);
        self.gateway.received(&self.connection, self.behaviour);
        Ok(bytes.len())
    }

    async fn flush(&mut self) -> Result<(), PipeError> {
        Ok(())
    }
}

/// Receive slots over the fake gateway.
#[derive(Clone)]
struct TestSlots {
    gateway: Rc<FakeGateway>,
    capacity: usize,
    in_use: Rc<Cell<usize>>,
}

struct TestLease {
    gateway: Rc<FakeGateway>,
    in_use: Rc<Cell<usize>>,
}

impl Drop for TestLease {
    fn drop(&mut self) {
        self.in_use.set(self.in_use.get() - 1);
    }
}

impl ReceiveSlotSource for TestSlots {
    type Lease = TestLease;

    fn acquire(&self) -> Option<TestLease> {
        if self.in_use.get() >= self.capacity {
            return None;
        }
        self.in_use.set(self.in_use.get() + 1);
        Some(TestLease {
            gateway: Rc::clone(&self.gateway),
            in_use: Rc::clone(&self.in_use),
        })
    }

    fn capacity(&self) -> usize {
        self.capacity
    }

    fn in_use(&self) -> usize {
        self.in_use.get()
    }
}

impl<Storage: PluginStorage> ReceiveChannel<TestLease> for Channel<Storage> {
    fn receive<'a>(
        &'a self,
        lease: &'a mut TestLease,
        session: ReceiveSession<'a>,
    ) -> ReceiveFuture<'a> {
        Box::pin(async move {
            let (settings, url, state) = self.prepare().await?;
            assert_eq!(url, "wss://gateway.qq.test/websocket/");
            let connection = lease.gateway.accept();
            let behaviour = lease.gateway.current.borrow().as_ref().expect("current").1;
            let reader = ConnectionReader(Rc::clone(&connection));
            let writer = ConnectionWriter {
                connection,
                gateway: Rc::clone(&lease.gateway),
                behaviour,
            };
            let outcome = self
                .run(&settings, state, &url, reader, writer, session)
                .await;
            if outcome.is_err() {
                lease.gateway.connection().end();
            }
            outcome.map_err(|error: ReceiveError| error)
        })
    }
}

// --- Harness ---------------------------------------------------------------

struct OccupiedQQChannel;

impl MessageChannel for OccupiedQQChannel {
    fn channel(&self) -> &str {
        "qq"
    }

    fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::SendMessage)) })
    }
}

/// What the probe stores and scripts before building the channel.
struct Setup {
    configured: bool,
    mode: Option<&'static str>,
    stored: Vec<(&'static str, Vec<u8>)>,
    /// OpenAPI answers: tokens, gateway URL, and sends.
    send: Vec<ScriptStep>,
    behaviours: Vec<Behaviour>,
    hold_slot: bool,
    occupied: bool,
}

impl Setup {
    fn configured(mode: &'static str) -> Self {
        Self {
            configured: true,
            mode: Some(mode),
            send: vec![ScriptStep::json(200, TOKEN), ScriptStep::json(200, GATEWAY)],
            ..Self::unconfigured()
        }
    }

    fn unconfigured() -> Self {
        Self {
            configured: false,
            mode: None,
            stored: Vec::new(),
            send: Vec::new(),
            behaviours: Vec::new(),
            hold_slot: false,
            occupied: false,
        }
    }
}

struct Harness<Storage: PluginStorage> {
    channel: Rc<Channel<Storage>>,
    runtime: RefCell<Option<ReceiveRuntime>>,
    gateway: Rc<FakeGateway>,
    send: &'static ScriptedStack,
    held: RefCell<Option<TestLease>>,
    registry: Rc<IMessageGateway>,
    storage: Storage,
    /// Another `qq` channel holding the name while present.
    _occupied: Option<barracuda_imessage_gateway_plugin::MessageChannelRegistration>,
}

impl<Storage: PluginStorage> Harness<Storage> {
    /// Drives the receive runtime until `done` holds; fails after 5 s.
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
            "timed out waiting for {what}: state {:?}, client sent {:?}",
            self.state(),
            self.gateway.texts.borrow()
        );
    }

    fn run_for(&self, duration: Duration) {
        let mut runtime = self.runtime.borrow_mut();
        let runtime = runtime.as_mut().expect("receive runtime");
        let _elapsed = block_on(select(runtime, Timer::after(duration)));
    }

    fn state(&self) -> ReceiveState {
        ChannelControl::receive(&*self.channel).state()
    }

    fn status(&self) -> Value {
        let endpoint = ChannelEndpoint::new(
            Rc::clone(&self.channel),
            CONFIG_API_PATH,
            ConfigEndpoint {
                channel: Rc::clone(&self.channel),
            },
        );
        let request = HttpRequest::with_path(
            HttpMethod::Get,
            format!("{CONFIG_API_PATH}/status"),
            Vec::new(),
        );
        let response = block_on(endpoint.handle(request));
        assert_eq!(response.status(), 200);
        serde_json::from_slice(response.body().expect("buffered body")).expect("status JSON")
    }

    fn configure(&self, body: &str) -> HttpResponse {
        let endpoint = ConfigEndpoint {
            channel: Rc::clone(&self.channel),
        };
        block_on(endpoint.handle(HttpRequest::new(HttpMethod::Post, body.as_bytes().to_vec())))
    }

    fn set_mode(&self, mode: &str) -> HttpResponse {
        let endpoint = ModeEndpoint::new(Rc::clone(&self.channel));
        let body = format!(r#"{{"mode":"{mode}"}}"#);
        block_on(endpoint.handle(HttpRequest::new(HttpMethod::Post, body.into_bytes())))
    }

    fn stored(&self, key: &str) -> Option<Vec<u8>> {
        block_on(self.storage.get_bytes(key)).expect("read storage")
    }

    fn stored_session(&self) -> Option<Value> {
        self.stored(SESSION_STORAGE_KEY)
            .map(|bytes| serde_json::from_slice(&bytes).expect("session JSON"))
    }

    /// Whether a `qq` channel is registered with the Gateway.
    fn registered(&self) -> bool {
        let probe: Rc<dyn MessageChannel> = Rc::new(OccupiedQQChannel);
        self.registry.register(probe).is_err()
    }

    fn requests(&self) -> Vec<String> {
        self.send.requests()
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
    const ID: &'static str = "qq-probe";
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
        let registry = context.require::<IMessageGateway>("imessage-gateway")?;
        let occupied = setup.occupied.then(|| {
            let occupied: Rc<dyn MessageChannel> = Rc::new(OccupiedQQChannel);
            registry.register(occupied).expect("occupy the qq channel")
        });
        let send: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new(setup.send)));
        let gateway = Rc::new(FakeGateway::default());
        gateway.script(setup.behaviours);
        let slots = TestSlots {
            gateway: Rc::clone(&gateway),
            capacity: 1,
            in_use: Rc::new(Cell::new(0)),
        };
        let held = setup
            .hold_slot
            .then(|| slots.acquire().expect("a free slot"));
        let (channel, runtime) = build_channel(
            storage.clone(),
            Rc::clone(&registry),
            ClientFactory::from_network(send, send),
            slots,
            entropy(),
            TIMING,
        )?;
        self.runtime = self.scenario.run(Harness {
            channel,
            runtime: RefCell::new(Some(runtime)),
            gateway,
            send,
            held: RefCell::new(held),
            registry,
            storage,
            _occupied: occupied,
        });
        self.completed.set(true);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        if let Some(runtime) = self.runtime.take() {
            let task = qq_receive_task(runtime, context.task_token())
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

/// Registers Workflow, the Gateway, and a probe running `scenario`, without
/// starting any task.
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
        .expect("run the QQ scenario");
    assert!(completed.get());
}

// --- Configuration ---------------------------------------------------------

struct AcceptedConfiguration;

impl Scenario for AcceptedConfiguration {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        assert_eq!(
            harness.status(),
            json!({"configured": false, "mode": "send_receive",
                   "receive": {"state": "idle", "slots": {"in_use": 0, "capacity": 1}},
                   "owners": {"count": 0}})
        );
        assert!(!harness.registered());
        let response = harness.configure(
            r#"{"app_id":"app","app_secret":"secret","api_base":"http://qq.test","token_url":"http://qq.test/app/getAppAccessToken"}"#,
        );
        assert_eq!(response.status(), 204);
        let stored = harness.stored(CONFIGURATION_STORAGE_KEY).expect("stored");
        assert!(!String::from_utf8_lossy(&stored).contains("token-1"));
        assert!(harness.registered());
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        assert_eq!(
            harness.status(),
            json!({"configured": true, "mode": "send_receive",
                   "receive": {"state": "receiving", "slots": {"in_use": 1, "capacity": 1}},
                   "owners": {"count": 0}})
        );
        let requests = harness.requests();
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert!(requests[0].starts_with("POST /app/getAppAccessToken HTTP/1.1"));
        // The token fetched by verification is reused for the gateway lookup.
        assert!(requests[1].starts_with("GET /gateway HTTP/1.1"));
        assert!(requests[1].contains("Authorization: QQBot token-1"));
        let identify = harness.gateway.texts.borrow()[0].clone();
        assert_eq!(identify["op"], 2);
        assert_eq!(identify["d"]["token"], "QQBot token-1");
        assert_eq!(identify["d"]["intents"], 1 << 25);
        assert_eq!(
            harness.stored_session(),
            Some(json!({"session_id": "session-1", "seq": 1}))
        );
        assert_eq!(
            entry_status(&*harness.channel),
            EntryStatus::new(
                EntryState::Ready,
                barracuda_captive_portal_plugin::WebText {
                    zh: "收发中",
                    en: "Receiving"
                }
            )
        );
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

#[test]
fn a_new_configuration_registers_identifies_and_receives() {
    run(
        Setup {
            send: vec![ScriptStep::json(200, TOKEN), ScriptStep::json(200, GATEWAY)],
            ..Setup::unconfigured()
        },
        AcceptedConfiguration,
    );
}

struct RejectedCredentials;

impl Scenario for RejectedCredentials {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        let response = harness.configure(CONFIG);
        assert_eq!(response.status(), 422);
        assert_eq!(
            response.body().unwrap(),
            r#"{"error":"verification_failed","message":"机器人不存在","code":"10004"}"#.as_bytes()
        );
        assert!(harness.stored(CONFIGURATION_STORAGE_KEY).is_none());
        assert!(!harness.registered());
        let response = harness.configure(CONFIG);
        assert_eq!(response.status(), 502);
        assert!(response
            .body()
            .unwrap()
            .starts_with(br#"{"error":"upstream_unavailable","message":"#));
        let response = harness.configure(r#"{"app_id":"app","access_token":"old"}"#);
        assert_eq!(response.status(), 400);
        assert_eq!(harness.requests().len(), 2);
        None
    }
}

#[test]
fn rejected_or_unverifiable_credentials_store_nothing() {
    run(
        Setup {
            send: vec![
                ScriptStep::json(200, REJECTED),
                ScriptStep::response(502, "text/html", b"<html>bad gateway</html>", usize::MAX),
            ],
            ..Setup::unconfigured()
        },
        RejectedCredentials,
    );
}

struct RegistrationRejected;

impl Scenario for RegistrationRejected {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        let response = harness.configure(CONFIG);
        assert_eq!(response.status(), 422);
        assert_eq!(
            response.body().unwrap(),
            br#"{"error":"registration_failed"}"#
        );
        assert!(harness.stored(CONFIGURATION_STORAGE_KEY).is_none());
        assert!(!harness.channel.configured());
        None
    }
}

#[test]
fn rejected_channel_registration_is_registration_failed() {
    run(
        Setup {
            send: vec![ScriptStep::json(200, TOKEN)],
            occupied: true,
            ..Setup::unconfigured()
        },
        RegistrationRejected,
    );
}

struct LegacyConfiguration;

impl Scenario for LegacyConfiguration {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        assert_eq!(
            harness.status(),
            json!({"configured": true, "mode": "send", "owners": {"count": 0}})
        );
        assert_eq!(
            harness.stored(MODE_STORAGE_KEY).as_deref(),
            Some(&br#""send""#[..])
        );
        assert!(harness.registered());
        harness.run_for(Duration::from_millis(50));
        assert_eq!(harness.gateway.connections.get(), 0);
        assert!(harness.requests().is_empty());
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
        LegacyConfiguration,
    );
}

struct LegacyToken;

impl Scenario for LegacyToken {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        assert!(!harness.channel.configured());
        assert!(!harness.registered());
        assert!(harness.stored(CONFIGURATION_STORAGE_KEY).is_some());
        None
    }
}

#[test]
fn a_stored_access_token_configuration_leaves_qq_unconfigured() {
    run(
        Setup {
            stored: vec![(
                CONFIGURATION_STORAGE_KEY,
                br#"{"app_id":"app","access_token":"old-token","api_base":"https://api.sgroup.qq.com"}"#
                    .to_vec(),
            )],
            ..Setup::unconfigured()
        },
        LegacyToken,
    );
}

#[test]
fn stored_configuration_round_trips_every_field() -> Result<(), serde_json::Error> {
    let config = ConfigRequest {
        app_id: "app".into(),
        app_secret: "secret".into(),
        api_base: "https://qq.example".into(),
        token_url: "https://qq.example/token".into(),
    };
    let restored = decode_configuration(&serde_json::to_vec(&config)?)?;
    assert_eq!(restored.app_id, config.app_id);
    assert_eq!(restored.app_secret, config.app_secret);
    assert_eq!(restored.api_base, config.api_base);
    assert_eq!(restored.token_url, config.token_url);
    let defaults: ConfigRequest =
        serde_json::from_slice(br#"{"app_id":"app","app_secret":"secret"}"#)?;
    assert_eq!(defaults.api_base, "https://api.sgroup.qq.com");
    assert_eq!(
        defaults.token_url,
        "https://bots.qq.com/app/getAppAccessToken"
    );
    Ok(())
}

// --- Modes and slots -------------------------------------------------------

struct Modes;

impl Scenario for Modes {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        assert_eq!(harness.set_mode("send").status(), 204);
        harness.run_until("idle", || harness.state() == ReceiveState::Idle);
        assert_eq!(ChannelControl::receive(&*harness.channel).in_use(), 0);
        assert!(harness.registered());
        assert_eq!(
            harness.status(),
            json!({"configured": true, "mode": "send", "owners": {"count": 0}})
        );
        assert_eq!(harness.set_mode("disabled").status(), 204);
        assert!(!harness.registered());
        assert_eq!(harness.set_mode("send_receive").status(), 204);
        assert!(harness.registered());
        harness.run_until("receiving again", || {
            harness.state() == ReceiveState::Receiving && harness.gateway.connections.get() == 2
        });
        // The session from the first connection is resumed.
        assert_eq!(harness.gateway.ops(), [2, 6]);
        // The gateway URL is cached: one token and one lookup in all.
        assert_eq!(harness.requests().len(), 2);
        None
    }
}

#[test]
fn modes_start_stop_and_register_the_channel() {
    run(Setup::configured("send_receive"), Modes);
}

struct FullPool;

impl Scenario for FullPool {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        assert_eq!(harness.state(), ReceiveState::NoSlot);
        assert_eq!(harness.status()["receive"]["state"], "no_slot");
        assert_eq!(harness.status()["receive"]["capacity"], 1);
        assert_eq!(harness.set_mode("send").status(), 204);
        let response = harness.set_mode("send_receive");
        assert_eq!(response.status(), 409);
        assert_eq!(
            response.body().unwrap(),
            br#"{"error":"no_slot","capacity":1}"#
        );
        harness.run_for(Duration::from_millis(60));
        assert_eq!(harness.gateway.connections.get(), 0);
        harness.held.borrow_mut().take();
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        None
    }
}

#[test]
fn a_full_pool_reports_no_slot_and_starts_when_a_slot_frees() {
    run(
        Setup {
            hold_slot: true,
            ..Setup::configured("send_receive")
        },
        FullPool,
    );
}

// --- Gateway protocol ------------------------------------------------------

struct Heartbeats;

impl Scenario for Heartbeats {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        let gateway = &harness.gateway;
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        let seq = gateway.dispatch("GUILD_CREATE", json!({}));
        // A heartbeat goes out while the read is pending, with the last seq.
        harness.run_until("a heartbeat", || gateway.ops().contains(&1));
        let heartbeat = gateway.texts.borrow().last().cloned().unwrap();
        assert_eq!(heartbeat, json!({"op": 1, "d": seq}));
        // 4009: reconnect at once and resume with the stored session.
        gateway.close(4009);
        harness.run_until("a resume", || gateway.ops().contains(&6));
        let resume = gateway
            .texts
            .borrow()
            .iter()
            .find(|text| text["op"] == 6)
            .cloned()
            .unwrap();
        assert_eq!(resume["d"]["session_id"], "session-1");
        assert_eq!(resume["d"]["seq"], seq);
        assert_eq!(gateway.client_closes.borrow().as_slice(), [1000]);
        harness.run_until("receiving after resume", || {
            harness.state() == ReceiveState::Receiving
        });
        // 4006: the session is gone; identify anew after a short backoff.
        gateway.close(4006);
        harness.run_until("a new identify", || {
            gateway.ops().iter().filter(|op| **op == 2).count() == 2
        });
        // The server's op 7 asks for a reconnect and resume.
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        gateway.connection().push(&text_frame(&json!({"op": 7})));
        harness.run_until("a second resume", || {
            gateway.ops().iter().filter(|op| **op == 6).count() == 2
        });
        // Invalid session without resume: identify again.
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        gateway
            .connection()
            .push(&text_frame(&json!({"op": 9, "d": false})));
        harness.run_until("a third identify", || {
            gateway.ops().iter().filter(|op| **op == 2).count() == 3
        });
        // The gateway URL was looked up once.
        assert_eq!(harness.requests().len(), 2);
        // 4914 halts with a message until reconfigured.
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        gateway.close(4914);
        harness.run_until("halted", || {
            matches!(harness.state(), ReceiveState::Error(_))
        });
        let connections = gateway.connections.get();
        harness.run_for(Duration::from_millis(100));
        assert_eq!(gateway.connections.get(), connections);
        assert_eq!(
            harness.state(),
            ReceiveState::Error(qq::gateway::DELISTED.into())
        );
        assert_eq!(
            harness.status()["receive"]["message"],
            qq::gateway::DELISTED
        );
        None
    }
}

#[test]
fn heartbeats_resume_reidentify_and_halt_by_close_code() {
    run(
        Setup {
            behaviours: vec![Behaviour {
                hello_interval_ms: 1000,
                ..Behaviour::default()
            }],
            ..Setup::configured("send_receive")
        },
        Heartbeats,
    );
}

struct MissedAck;

impl Scenario for MissedAck {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        let gateway = &harness.gateway;
        harness.run_until("a resume after two unacknowledged heartbeats", || {
            gateway.ops().contains(&6)
        });
        assert_eq!(gateway.ops(), [2, 1, 6]);
        None
    }
}

#[test]
fn an_unacknowledged_heartbeat_reconnects_and_resumes() {
    run(
        Setup {
            behaviours: vec![Behaviour {
                hello_interval_ms: 1000,
                ack: false,
                ..Behaviour::default()
            }],
            ..Setup::configured("send_receive")
        },
        MissedAck,
    );
}

struct StoredSession;

impl Scenario for StoredSession {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        let resume = harness.gateway.texts.borrow()[0].clone();
        assert_eq!(
            resume,
            json!({"op": 6, "d": {"token": "QQBot token-1", "session_id": "old", "seq": 41}})
        );
        None
    }
}

#[test]
fn a_stored_session_is_resumed_after_a_restart() {
    run(
        Setup {
            stored: vec![(
                SESSION_STORAGE_KEY,
                br#"{"session_id":"old","seq":41}"#.to_vec(),
            )],
            ..Setup::configured("send_receive")
        },
        StoredSession,
    );
}

struct RefusedUpgrade;

impl Scenario for RefusedUpgrade {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("the gateway rate limit", || {
            harness
                .state()
                .message()
                .is_some_and(|message| message.contains("waiting to ask QQ"))
        });
        assert_eq!(harness.gateway.connections.get(), 1);
        // The stale URL was dropped, but the lookup waits for QQ's rate limit.
        let state = harness.channel.receiving.get().expect("loaded");
        assert!(state.gateway_url.borrow().is_none());
        assert_eq!(harness.requests().len(), 2);
        None
    }
}

#[test]
fn a_refused_upgrade_drops_the_url_and_respects_the_lookup_rate_limit() {
    run(
        Setup {
            behaviours: vec![Behaviour {
                refuse_upgrade: true,
                ..Behaviour::default()
            }],
            ..Setup::configured("send_receive")
        },
        RefusedUpgrade,
    );
}

struct BadSecret;

impl Scenario for BadSecret {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        harness.run_until("halted", || {
            matches!(harness.state(), ReceiveState::Error(_))
        });
        harness.run_for(Duration::from_millis(100));
        assert_eq!(harness.requests().len(), 1);
        assert!(harness.state().message().unwrap().contains("机器人不存在"));
        None
    }
}

#[test]
fn a_rejected_app_secret_halts_receiving() {
    run(
        Setup {
            send: vec![ScriptStep::json(200, REJECTED)],
            ..Setup::configured("send_receive")
        },
        BadSecret,
    );
}

// --- Owners, publishing, and dedup with Workflow running --------------------

/// Records every `gateway.message.received` a Workflow hands it.
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
    static WORLD: RefCell<Option<(Rc<FakeGateway>, &'static ScriptedStack)>> =
        const { RefCell::new(None) };
}

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
            ignored: channel.owners().expect("owners").ignored(),
        })));
        WORLD.with(|world| world.replace(Some((Rc::clone(&harness.gateway), harness.send))));
        harness.runtime.take()
    }
}

fn c2c(id: &str, sender: &str, text: &str) -> Value {
    json!({"id": id, "author": {"id": sender, "user_openid": sender}, "content": text,
           "timestamp": "2026-07-21T10:00:00+08:00"})
}

#[embassy_executor::task]
async fn end_to_end(spawner: Spawner, done: SyncSender<Result<(), String>>) {
    let outcome = end_to_end_steps(spawner).await;
    let _sent = done.send(outcome);
}

async fn wait_for(what: &str, done: impl Fn() -> bool) -> Result<(), String> {
    with_timeout(Duration::from_secs(5), async {
        while !done() {
            Timer::after_millis(5).await;
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for {what}"))
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
    manager
        .register(Probe {
            setup: Some(Setup {
                stored: vec![(
                    "owners",
                    br#"[{"id":"OWNER","label":"Ann"},{"id":"MEMBER"}]"#.to_vec(),
                )],
                send: vec![
                    ScriptStep::json(200, TOKEN),
                    ScriptStep::json(200, GATEWAY),
                    ScriptStep::json(200, MESSAGE_SENT),
                ],
                ..Setup::configured("send_receive")
            }),
            scenario: Started(Rc::clone(&snapshots)),
            runtime: None,
            completed: Rc::new(Cell::new(false)),
        })
        .map_err(|error| error.to_string())?;
    manager.start().map_err(|error| error.to_string())?;

    let (gateway, send) = WORLD
        .with(|world| world.borrow().clone())
        .ok_or("the scenario's world")?;
    wait_for("the first login", || gateway.ops() == [2]).await?;
    Timer::after_millis(20).await;
    gateway.dispatch("C2C_MESSAGE_CREATE", c2c("m1", "OWNER", " hello "));
    gateway.dispatch("C2C_MESSAGE_CREATE", c2c("m2", "STRANGER", "hi"));
    gateway.dispatch("C2C_MESSAGE_CREATE", c2c("m3", "NEWCOMER", PAIRING_CODE));
    gateway.dispatch(
        "GROUP_AT_MESSAGE_CREATE",
        json!({"id": "g1", "author": {"member_openid": "MEMBER"}, "content": " status",
               "group_openid": "GROUP"}),
    );
    gateway.dispatch(
        "C2C_MESSAGE_CREATE",
        json!({"id": "m4", "author": {"user_openid": "OWNER"}, "content": "",
               "attachments": [{"content_type": "image/png"}]}),
    );
    // The connection drops; after the resume QQ delivers m1 again.
    wait_for("three messages", || records.borrow().len() == 2).await?;
    gateway.close(4009);
    wait_for("the resume", || gateway.ops() == [2, 6]).await?;
    Timer::after_millis(20).await;
    gateway.dispatch("C2C_MESSAGE_CREATE", c2c("m1", "OWNER", " hello "));
    gateway.dispatch("C2C_MESSAGE_CREATE", c2c("m5", "OWNER", "again"));
    wait_for("the last message", || records.borrow().len() >= 3).await?;
    Timer::after_millis(30).await;

    let records = records.borrow().clone();
    let snapshot = (snapshots.borrow().as_ref().ok_or("snapshot")?)();
    let check = |condition: bool, what: &str| {
        if condition {
            Ok(())
        } else {
            Err(format!(
                "{what}: records {records:?}, sent {:?}",
                send.requests()
            ))
        }
    };
    check(
        records
            == vec![
                json!({"route": {"channel": "qq", "conversation_id": "c2c:OWNER"}, "message_id": "m1", "text": "hello"}),
                json!({"route": {"channel": "qq", "conversation_id": "group:GROUP"}, "message_id": "g1", "text": "status"}),
                json!({"route": {"channel": "qq", "conversation_id": "c2c:OWNER"}, "message_id": "m5", "text": "again"}),
            ],
        "owner messages are published once each with routes the sender accepts",
    )?;
    let sent = send.requests();
    check(sent.len() == 3, "one paired reply")?;
    let (head, body) = sent[2].split_once("\r\n\r\n").ok_or("request body")?;
    let body: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    check(
        head.starts_with("POST /v2/users/NEWCOMER/messages "),
        "the reply goes to the new owner's chat",
    )?;
    check(
        body == json!({"content": PAIRED_REPLY, "msg_type": 0, "msg_id": "m3", "msg_seq": 1}),
        "the reply is a passive reply to the code message",
    )?;
    check(
        snapshot.owners == ["OWNER", "MEMBER", "NEWCOMER"],
        "the sender with the code paired",
    )?;
    check(snapshot.ignored == 1, "the stranger is counted")?;
    Ok(())
}

#[test]
fn owners_publish_codes_pair_strangers_are_ignored_and_redeliveries_dropped() {
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

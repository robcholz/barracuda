//! The QQ channel against a scripted OpenAPI and a fake QQ WebSocket gateway
//! on a loopback network, real Plugin storage, and the real Gateway, HTTP,
//! WebServer, and Workflow.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    missing_docs
)]

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use core::cell::{Cell, RefCell};
use core::future::{poll_fn, Future};
use core::pin::Pin;
use core::task::{Poll, Waker};
use std::sync::mpsc::{sync_channel, SyncSender};

use barracuda_captive_portal_plugin::{EntryState, EntryStatus};
use barracuda_http_plugin::HttpPlugin;
use barracuda_imessage_gateway_plugin::{
    ChannelControl, ChannelError, ChannelFuture, IMessageGatewayPlugin, MessageChannel,
    MessageChannelRegistration, Operation, ReceiveControl, ReceiveSlots, ReceiveState,
    SendMessageRequest, SendReceipt, PAIRED_REPLY,
};
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_platform_test::{
    install_global_memory_vfs, loopback_network, memory_partition, LoopbackNetwork,
    MemoryPartition, ScriptStep, ScriptedServer,
};
use barracuda_plugin::manager::{PluginDeclaration, PluginManager};
use barracuda_webserver_plugin::{WebServer, WebServerPlugin};
use barracuda_workflow_plugin::{
    workflow_action_schema_inline, WorkflowActionFuture, WorkflowActionHandler,
    WorkflowActionRegistry, WorkflowActionSchema, WorkflowPlugin, WorkflowService,
};
use embassy_executor::{Executor, Spawner};
use embassy_futures::join::{join3, join_array};
use embassy_futures::select::{select, Either};
use embassy_net::tcp::TcpSocket;
use embassy_net::Stack;
use embassy_time::{with_timeout, Duration, Timer};
use embedded_io_async::Write as _;
use futures_lite::future::block_on;
use rusty_fork::rusty_fork_test;
use serde_json::{json, Value};

use super::*;
use crate::receive::SESSION_STORAGE_KEY;

type Channel<Storage> = QQChannel<Storage>;

/// Port of the scripted OpenAPI, token endpoint, and bind portal.
const PORT: u16 = 9000;
/// Connections the scripted server holds at once. Every request opens a
/// fresh connection whose client side is dropped without a FIN, so each one
/// keeps a listener until the test ends.
const LISTENERS: usize = 10;
/// Port of the fake QQ WebSocket gateway.
const WS_PORT: u16 = 9001;
/// Connections the fake gateway accepts at once: the one ending and the next.
const WS_LISTENERS: usize = 2;
const CONFIG: &str = r#"{"app_id":"app","app_secret":"secret","api_base":"http://10.0.0.1:9000","token_url":"http://10.0.0.1:9000/app/getAppAccessToken"}"#;
const TOKEN: &str = r#"{"access_token":"token-1","expires_in":"7200"}"#;
const GATEWAY: &str = r#"{"url":"ws://10.0.0.1:9001/websocket/"}"#;
const MESSAGE_SENT: &str = r#"{"id":"reply-1"}"#;
const REJECTED: &str = r#"{"code":10004,"message":"机器人不存在"}"#;
const PAIRING_CODE: &str = "123456";
const TIMING: ReceiveTiming = ReceiveTiming {
    initial_backoff: Duration::from_millis(20),
    max_backoff: Duration::from_millis(80),
    slot_retry: Duration::from_millis(20),
};

// --- Entropy ---------------------------------------------------------------

/// Entropy that fills every four bytes of one draw with a counter, starting
/// at a seed, so the first draw is the seed.
#[derive(Clone)]
struct Sequence(portable_atomic_util::Arc<portable_atomic::AtomicU32>);

impl Entropy for Sequence {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        let value = self.0.fetch_add(1, portable_atomic::Ordering::Relaxed);
        for chunk in bytes.chunks_mut(4) {
            chunk.copy_from_slice(&value.to_le_bytes()[..chunk.len()]);
        }
        Ok(())
    }
}

/// Entropy whose first draw is [`PAIRING_CODE`]: the Gateway's first pairing
/// code, and the channel's first bind key.
fn entropy() -> SharedEntropy {
    SharedEntropy::new(Sequence(portable_atomic_util::Arc::new(
        portable_atomic::AtomicU32::new(123_456),
    )))
}

// --- Fake QQ WebSocket gateway ---------------------------------------------

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
        self.wake();
    }

    fn end(&self) {
        self.closed.set(true);
        self.wake();
    }

    fn wake(&self) {
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }

    /// Waits until there are bytes for the client or the connection ends.
    async fn outgoing(&self) {
        poll_fn(|context| {
            if self.closed.get() || !self.to_client.borrow().is_empty() {
                return Poll::Ready(());
            }
            self.waker.replace(Some(context.waker().clone()));
            Poll::Pending
        })
        .await;
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

/// The fake QQ gateway: a TCP server on the loopback network that speaks the
/// WebSocket protocol and records what clients send.
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

    /// Serves `port` on `stack` with `N` concurrent connections, forever.
    async fn serve<const N: usize>(&self, stack: Stack<'static>, port: u16) {
        join_array(core::array::from_fn::<_, N, _>(|_| {
            self.listen(stack, port)
        }))
        .await;
    }

    async fn listen(&self, stack: Stack<'static>, port: u16) {
        let mut rx = vec![0; 8 * 1024];
        let mut tx = vec![0; 8 * 1024];
        loop {
            let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
            if socket.accept(port).await.is_err() {
                continue;
            }
            let (connection, behaviour) = self.accept();
            self.pump(&mut socket, &connection, behaviour).await;
            connection.end();
            socket.abort();
            let _flushed = socket.flush().await;
        }
    }

    /// Moves bytes between `socket` and `connection` until either ends.
    async fn pump(
        &self,
        socket: &mut TcpSocket<'_>,
        connection: &Connection,
        behaviour: Behaviour,
    ) {
        let mut buffer = [0; 1024];
        loop {
            let outgoing: Vec<u8> = connection.to_client.borrow_mut().drain(..).collect();
            if !outgoing.is_empty()
                && (socket.write_all(&outgoing).await.is_err() || socket.flush().await.is_err())
            {
                return;
            }
            if connection.closed.get() {
                return;
            }
            match select(socket.read(&mut buffer), connection.outgoing()).await {
                Either::First(Ok(0) | Err(_)) => return,
                Either::First(Ok(count)) => {
                    connection
                        .from_client
                        .borrow_mut()
                        .extend_from_slice(&buffer[..count]);
                    self.received(connection, behaviour);
                }
                Either::Second(()) => {}
            }
        }
    }

    /// Makes a new connection the current one; the previous one ends.
    fn accept(&self) -> (Rc<Connection>, Behaviour) {
        let connection = Rc::new(Connection::default());
        let behaviour = self.behaviour.borrow_mut().pop_front().unwrap_or_default();
        self.connections.set(self.connections.get() + 1);
        if let Some((previous, _)) = self
            .current
            .replace(Some((Rc::clone(&connection), behaviour)))
        {
            previous.end();
        }
        (connection, behaviour)
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
            assert!(
                head.starts_with("GET /websocket/ HTTP/1.1\r\nHost: 10.0.0.1:9001\r\n"),
                "{head}"
            );
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

/// What the probe stores before building the channel, and what the QQ
/// servers answer.
struct Setup {
    configured: bool,
    mode: Option<&'static str>,
    stored: Vec<(&'static str, Vec<u8>)>,
    /// OpenAPI and portal answers, in order: tokens, gateway URL, sends, and
    /// bind calls.
    send: Vec<ScriptStep>,
    behaviours: Vec<Behaviour>,
    /// Whether other receivers hold every Gateway slot.
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

/// The loopback network, the scripted OpenAPI, and the fake gateway, all
/// running whenever a test waits.
struct Bench {
    /// Answers the OpenAPI, the token endpoint, and the bind portal.
    server: ScriptedServer,
    gateway: Rc<FakeGateway>,
    driver: RefCell<Pin<Box<dyn Future<Output = ()>>>>,
}

impl Bench {
    fn new(network: LoopbackNetwork, setup: &mut Setup) -> Rc<Self> {
        let stack = network.stack();
        let server = ScriptedServer::new(core::mem::take(&mut setup.send));
        let gateway = Rc::new(FakeGateway::default());
        gateway.script(core::mem::take(&mut setup.behaviours));
        let serving = server.clone();
        let accepting = Rc::clone(&gateway);
        let driver: Pin<Box<dyn Future<Output = ()>>> = Box::pin(async move {
            let _stopped = join3(
                network.run(),
                serving.run::<LISTENERS>(stack, PORT),
                accepting.serve::<WS_LISTENERS>(stack, WS_PORT),
            )
            .await;
        });
        Rc::new(Self {
            server,
            gateway,
            driver: RefCell::new(driver),
        })
    }

    /// Runs `future` to its end while the network and the QQ servers run.
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

    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).expect("JSON body")
        }
    }
}

/// Requests `method path` with `body` from the WebServer's routes.
async fn call(webserver: &WebServer, method: &str, path: &str, body: &str) -> Reply {
    let mut input = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    input.extend_from_slice(body.as_bytes());
    let output = routes::send(webserver, input).await;
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

/// The built channel, its routes, and the QQ servers.
struct Harness<Storage: PluginStorage> {
    channel: Rc<Channel<Storage>>,
    runtime: RefCell<Option<ReceiveRuntime>>,
    bench: Rc<Bench>,
    /// The fake QQ WebSocket gateway.
    gateway: Rc<FakeGateway>,
    webserver: Rc<WebServer>,
    /// Receivers holding every Gateway slot, when the setup asks for it.
    held: RefCell<Vec<ReceiveControl<ReceiveSlots>>>,
    /// The Gateway's slot count.
    capacity: usize,
    registry: Rc<IMessageGateway>,
    storage: Storage,
    /// Another `qq` channel holding the name while present.
    _occupied: Option<MessageChannelRegistration>,
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
            "timed out waiting for {what}: state {:?}, client sent {:?}",
            self.state(),
            self.gateway.texts.borrow()
        );
    }

    /// Drives the receive runtime for `duration`.
    fn run_for(&self, duration: Duration) {
        let mut runtime = self.runtime.borrow_mut();
        let runtime = runtime.as_mut().expect("receive runtime");
        let _elapsed = self.bench.drive(select(runtime, Timer::after(duration)));
    }

    fn state(&self) -> ReceiveState {
        ChannelControl::receive(&*self.channel).state()
    }

    /// Requests the channel's routes on the WebServer.
    fn request(&self, method: &str, path: &str, body: &str) -> Reply {
        self.bench.drive(call(&self.webserver, method, path, body))
    }

    fn status(&self) -> Value {
        let reply = self.request("GET", &format!("{CONFIG_API_PATH}/status"), "");
        assert_eq!(reply.status(), 200);
        reply.json()
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

    fn stored_session(&self) -> Option<Value> {
        self.stored(SESSION_STORAGE_KEY)
            .map(|bytes| serde_json::from_slice(&bytes).expect("session JSON"))
    }

    /// Whether a `qq` channel is registered with the Gateway.
    fn registered(&self) -> bool {
        let probe: Rc<dyn MessageChannel> = Rc::new(OccupiedQQChannel);
        self.registry.register(probe).is_err()
    }

    /// Requests the OpenAPI and the portal answered, in order.
    fn requests(&self) -> Vec<String> {
        self.bench.server.requests()
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
    const ID: &'static str = "qq-probe";
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
        let registry = context.require::<IMessageGateway>("imessage-gateway")?;
        let webserver = context.require::<WebServer>("webserver")?;
        let occupied = setup.occupied.then(|| {
            let occupied: Rc<dyn MessageChannel> = Rc::new(OccupiedQQChannel);
            registry.register(occupied).expect("occupy the qq channel")
        });
        let capacity = registry.receive_control().capacity();
        let held = if setup.hold_slot {
            (0..capacity)
                .map(|_| {
                    let control = registry.receive_control();
                    control.set_enabled(true).expect("a free slot");
                    control
                })
                .collect()
        } else {
            Vec::new()
        };
        let (channel, runtime) =
            build_channel(storage.clone(), Rc::clone(&registry), entropy(), TIMING)?;
        let config = ConfigEndpoint {
            channel: Rc::clone(&channel),
        };
        context.retain(
            registry
                .serve_channel(CONFIG_API_PATH, Rc::clone(&channel), config)
                .map_err(PluginError::registration)?,
        );
        self.runtime = self.scenario.run(Harness {
            channel,
            runtime: RefCell::new(Some(runtime)),
            gateway: Rc::clone(&self.bench.gateway),
            bench: Rc::clone(&self.bench),
            webserver,
            held: RefCell::new(held),
            capacity,
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

/// A context on `network`'s stack.
fn plugin_context(network: &LoopbackNetwork) -> PluginContext {
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
        .expect("run the QQ scenario");
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
    context.entropy = entropy();
    manager
        .register(IMessageGatewayPlugin::new(context))
        .expect("register IMessage Gateway Plugin");
}

// --- Configuration ---------------------------------------------------------

struct AcceptedConfiguration;

impl Scenario for AcceptedConfiguration {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        assert_eq!(
            harness.status(),
            json!({"configured": false, "mode": "send_receive",
                   "receive": {"state": "idle", "slots": {"in_use": 0, "capacity": harness.capacity}},
                   "owners": {"count": 0}})
        );
        assert!(!harness.registered());
        let response = harness.configure(CONFIG);
        assert_eq!(response.status(), 204);
        let stored = harness.stored(CONFIGURATION_STORAGE_KEY).expect("stored");
        assert!(!String::from_utf8_lossy(&stored).contains("token-1"));
        assert!(harness.registered());
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
        assert_eq!(
            harness.status(),
            json!({"configured": true, "mode": "send_receive",
                   "receive": {"state": "receiving", "slots": {"in_use": 1, "capacity": harness.capacity}},
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
            entry_status(harness.registry.channel_status(&*harness.channel)),
            EntryStatus::new(
                EntryState::Ready,
                barracuda_captive_portal_plugin::WebText {
                    zh: "收发中",
                    en: "Receiving"
                }
            )
        );
        // The configuration path takes changes only; its state is under `/status`.
        for method in ["GET", "DELETE"] {
            assert_eq!(harness.request(method, CONFIG_API_PATH, "").status(), 405);
        }
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
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

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
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

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
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
        assert_eq!(harness.gateway.connections.get(), 0);
        assert!(harness.requests().is_empty());
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
                mode: None,
                ..Setup::configured("send")
            },
            LegacyConfiguration,
        );
    }
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

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
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

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn modes_start_stop_and_register_the_channel() {
        run(Setup::configured("send_receive"), Modes);
    }
}

struct FullPool;

impl Scenario for FullPool {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        assert_eq!(harness.state(), ReceiveState::NoSlot);
        assert_eq!(harness.status()["receive"]["state"], "no_slot");
        assert_eq!(harness.status()["receive"]["capacity"], harness.capacity);
        assert_eq!(harness.set_mode("send").status(), 204);
        let response = harness.set_mode("send_receive");
        assert_eq!(response.status(), 409);
        assert_eq!(
            response.body().unwrap(),
            format!(r#"{{"error":"no_slot","capacity":{}}}"#, harness.capacity).as_bytes()
        );
        harness.run_for(Duration::from_millis(60));
        assert_eq!(harness.gateway.connections.get(), 0);
        drop(harness.held.take());
        harness.run_until("receiving", || harness.state() == ReceiveState::Receiving);
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
                hold_slot: true,
                ..Setup::configured("send_receive")
            },
            FullPool,
        );
    }
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

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
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

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
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

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
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

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
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

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
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
    static WORLD: RefCell<Option<(Rc<FakeGateway>, ScriptedServer)>> =
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
        // Keeps the QQ servers reachable from the end-to-end test.
        WORLD.with(|world| {
            world.replace(Some((
                Rc::clone(&harness.gateway),
                harness.bench.server.clone(),
            )))
        });
        harness.runtime.take()
    }
}

fn c2c(id: &str, sender: &str, text: &str) -> Value {
    json!({"id": id, "author": {"id": sender, "user_openid": sender}, "content": text,
           "timestamp": "2026-07-21T10:00:00+08:00"})
}

/// Runs the bench's network and QQ servers on the executor.
#[allow(
    clippy::await_holding_refcell_ref,
    reason = "the end-to-end test only waits on the executor, never through `Bench::drive`"
)]
#[embassy_executor::task]
async fn run_bench(bench: Rc<Bench>) {
    let mut driver = bench.driver.borrow_mut();
    driver.as_mut().await;
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
    let network = loopback_network();
    let mut context = plugin_context(&network);
    let records = Rc::new(RefCell::new(Vec::new()));
    let snapshots: Snapshots = Rc::new(RefCell::new(None));
    register_services(&mut manager, &mut context);
    manager
        .register(Recorder(Rc::clone(&records)))
        .map_err(|error| error.to_string())?;
    let mut setup = Setup {
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
    // The executor thread outlives the test; keep its Plugins running.
    core::mem::forget(manager);
    Ok(())
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn owners_publish_codes_pair_strangers_are_ignored_and_redeliveries_dropped() {
        let (done, outcome) = sync_channel(1);
        std::thread::Builder::new()
            .name("qq-end-to-end".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let executor = Box::leak(Box::new(Executor::new()));
                executor.run(|spawner| {
                    spawner.spawn(end_to_end(spawner, done).expect("spawn the end-to-end test"));
                });
            })
            .expect("spawn the executor thread");
        let outcome = outcome
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the end-to-end test finished");
        if let Err(message) = outcome {
            panic!("{message}");
        }
    }
}

// --- QR binding --------------------------------------------------------------

const PORTAL: &str = "http://10.0.0.1:9000";
/// `bound-secret` sealed to the first bind key the test entropy makes.
const SEALED_SECRET: &str = "BwcHBwcHBwcHBwcHfZEIFfZC1bTS4qY8MD2oy9+GDTnvVXKHf+S6Ow==";
const LOGIN_TIMING: LoginTiming = LoginTiming {
    session: Duration::from_secs(60),
    poll_interval: Duration::from_ticks(0),
};

fn bind_task() -> ScriptStep {
    ScriptStep::json(
        200,
        r#"{"retcode":0,"msg":"success","data":{"task_id":"task-1"}}"#,
    )
}

fn bind_status(status: u8) -> ScriptStep {
    ScriptStep::json(
        200,
        &json!({"retcode": 0, "data": {"status": status, "bot_appid": "0",
                "bot_encrypt_secret": "", "user_openid": ""}})
        .to_string(),
    )
}

fn bound() -> ScriptStep {
    ScriptStep::json(
        200,
        &json!({"retcode": 0, "data": {"status": 2, "bot_appid": "102345678",
                "bot_encrypt_secret": SEALED_SECRET, "user_openid": "SCANNER"}})
        .to_string(),
    )
}

/// The login endpoint on the WebServer and its runtime over the harness
/// channel, run side by side with `test` while the QQ servers run.
fn with_login<Storage: PluginStorage>(
    harness: &Harness<Storage>,
    test: impl AsyncFnOnce(&WebServer, &LoginWatch),
) {
    let (endpoint, runtime) =
        LoginEndpoint::new(Rc::clone(&harness.channel), PORTAL.into(), LOGIN_TIMING);
    let watch = endpoint.watch();
    let _route = harness
        .webserver
        .serve_http(LOGIN_API_PATH, endpoint)
        .expect("serve the login route");
    harness.bench.drive(async {
        match select(runtime, test(&harness.webserver, &watch)).await {
            Either::First(()) => panic!("login runtime ended"),
            Either::Second(()) => {}
        }
    });
}

/// Calls the login route without a body.
async fn login_call(webserver: &WebServer, method: &str) -> (u16, Value) {
    let reply = call(webserver, method, LOGIN_API_PATH, "").await;
    (reply.status(), reply.json())
}

/// Lets the runtime run until GET reports `expected`.
async fn login_status(webserver: &WebServer, expected: &str) -> Value {
    for _ in 0..10_000 {
        let (code, status) = login_call(webserver, "GET").await;
        assert_eq!(code, 200);
        if status["status"] == expected {
            return status;
        }
        Timer::after_millis(1).await;
    }
    panic!("the bind session never reported {expected}");
}

fn first_lines(requests: &[String]) -> Vec<&str> {
    requests
        .iter()
        .map(|request| request.lines().next().unwrap_or_default())
        .collect()
}

struct BoundByQr;

impl Scenario for BoundByQr {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        with_login(&harness, async |login, watch| {
            assert_eq!(
                login_call(login, "GET").await,
                (
                    200,
                    json!({"status": "idle", "configured": true, "app_id": "app"})
                )
            );
            let (code, body) = login_call(login, "POST").await;
            assert_eq!(code, 200);
            assert_eq!(
                body,
                json!({"url": "https://q.qq.com/qqbot/openclaw/connect.html?task_id=task-1&source=barracuda&_wv=2"})
            );
            let status = login_status(login, "confirmed").await;
            assert_eq!(
                status,
                json!({"status": "confirmed", "configured": true, "app_id": "102345678"})
            );
            assert!(!watch.waiting_for_scan());
        });
        let stored: Value =
            serde_json::from_slice(&harness.stored(CONFIGURATION_STORAGE_KEY).expect("stored"))
                .expect("configuration JSON");
        // The opened secret replaces the old bot; the endpoints stay.
        assert_eq!(
            stored,
            json!({"app_id": "102345678", "app_secret": "bound-secret",
                   "api_base": "http://10.0.0.1:9000",
                   "token_url": "http://10.0.0.1:9000/app/getAppAccessToken"})
        );
        assert!(harness.registered());
        assert_eq!(harness.status()["owners"], json!({"count": 1}));
        assert_eq!(
            harness.stored("owners"),
            Some(br#"[{"id":"SCANNER","label":null}]"#.to_vec())
        );
        let requests = harness.requests();
        assert_eq!(
            first_lines(&requests),
            [
                "POST /lite/create_bind_task HTTP/1.1",
                "POST /lite/poll_bind_result HTTP/1.1",
                "POST /lite/poll_bind_result HTTP/1.1",
                "POST /app/getAppAccessToken HTTP/1.1",
            ]
        );
        assert!(requests[0].ends_with(r#"{"key":"QOIBAEDiAQBA4gEAQOIBAEDiAQBA4gEAQOIBAEDiAQA="}"#));
        assert!(requests[3].contains(r#""appId":"102345678""#));
        assert!(requests[3].contains(r#""clientSecret":"bound-secret""#));
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_qr_binding_verifies_stores_the_bot_and_makes_the_scanner_an_owner() {
        run(
            Setup {
                send: vec![
                    bind_task(),
                    bind_status(1),
                    bound(),
                    ScriptStep::json(200, TOKEN),
                ],
                ..Setup::configured("send")
            },
            BoundByQr,
        );
    }
}

struct BindingRefused;

impl Scenario for BindingRefused {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        with_login(&harness, async |login, _watch| {
            assert_eq!(login_call(login, "POST").await.0, 200);
            let status = login_status(login, "failed").await;
            assert_eq!(status["configured"], true);
            assert!(status["message"]
                .as_str()
                .expect("message")
                .contains("10004"));
            // The QR code expires; a new one replaces it.
            assert_eq!(login_call(login, "POST").await.0, 200);
            login_status(login, "expired").await;
            // The portal refuses a task: its words come back.
            let (code, body) = login_call(login, "POST").await;
            assert_eq!(code, 502);
            assert_eq!(
                body,
                json!({"error": "upstream_unavailable", "message": "rate limited", "code": "429"})
            );
        });
        // The refused bot stored nothing; the old configuration stays.
        assert_eq!(
            harness.stored(CONFIGURATION_STORAGE_KEY),
            Some(CONFIG.as_bytes().to_vec())
        );
        assert_eq!(harness.status()["owners"], json!({"count": 0}));
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_refused_binding_stores_nothing_and_expired_or_refused_tasks_end_the_session() {
        run(
            Setup {
                send: vec![
                    bind_task(),
                    bound(),
                    ScriptStep::json(200, REJECTED),
                    bind_task(),
                    bind_status(1),
                    ScriptStep::response(502, "text/html", b"<html>bad gateway</html>", usize::MAX),
                    bind_status(3),
                    ScriptStep::json(200, r#"{"retcode":429,"msg":"rate limited"}"#),
                ],
                ..Setup::configured("send")
            },
            BindingRefused,
        );
    }
}

struct BindingCancelled;

impl Scenario for BindingCancelled {
    fn run<Storage: PluginStorage>(&self, harness: Harness<Storage>) -> Option<ReceiveRuntime> {
        with_login(&harness, async |login, watch| {
            let response = call(login, "POST", LOGIN_API_PATH, r#"{"x":1}"#).await;
            assert_eq!(response.status(), 400);
            assert_eq!(login_call(login, "POST").await.0, 200);
            assert_eq!(
                login_call(login, "GET").await,
                (200, json!({"status": "wait", "configured": false}))
            );
            assert!(watch.waiting_for_scan());
            assert_eq!(
                crate::card_status(&harness.channel, watch),
                EntryStatus::new(
                    EntryState::Attention,
                    barracuda_captive_portal_plugin::WebText {
                        zh: "等待扫码",
                        en: "Waiting for scan"
                    }
                )
            );
            assert_eq!(login_call(login, "DELETE").await.0, 204);
            assert_eq!(
                login_call(login, "GET").await,
                (200, json!({"status": "idle", "configured": false}))
            );
            assert!(!watch.waiting_for_scan());
            assert_eq!(login_call(login, "PUT").await.0, 405);
        });
        assert!(harness.stored(CONFIGURATION_STORAGE_KEY).is_none());
        None
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_cancelled_binding_returns_to_idle() {
        run(
            Setup {
                send: vec![
                    bind_task(),
                    ScriptStep::pending_after_headers(200, "application/json"),
                ],
                ..Setup::unconfigured()
            },
            BindingCancelled,
        );
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

//! The Inkbox channel against a scripted Inkbox API, real Plugin storage, and
//! the real Gateway, WebServer, HTTP, and Workflow on a loopback network.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs
)]

mod config;
mod receiving;

use alloc::format;
use alloc::string::ToString;
use core::future::Future;
use core::pin::Pin;

use barracuda_http_plugin::HttpPlugin;
use barracuda_imessage_gateway_plugin::{
    ChannelError, ChannelFuture, GatewaySendRequest, IMessageGatewayPlugin, Operation,
    ReceiveState, SendMessageRequest, SendReceipt,
};
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_platform_test::{
    install_global_memory_vfs, loopback_network, memory_partition, LoopbackNetwork,
    MemoryPartition, ScriptStep, ScriptedRoute, ScriptedServer,
};
use barracuda_plugin::api::SharedEntropy;
use barracuda_plugin::manager::{PluginDeclaration, PluginManager};
use barracuda_webserver_plugin::WebServerPlugin;
use barracuda_workflow_plugin::WorkflowPlugin;
use embassy_futures::join::join;
use embassy_futures::select::{select, Either};
use embassy_time::{with_timeout, Timer};
use futures_lite::future::block_on;
use serde_json::Value;

use super::*;

/// Port of the scripted Inkbox API on the loopback network.
const PORT: u16 = 9000;
/// Origin of the scripted Inkbox API, for signup and stored configurations.
const API_BASE: &str = "http://10.0.0.1:9000";
/// Configuration a receive scenario starts with.
const CONFIG: &str =
    r#"{"api_key":"key-1","identity_id":"identity-1","api_base":"http://10.0.0.1:9000"}"#;
/// Request line text of the channel's sends, which a route answers.
const SEND_ROUTE: &str = "POST /api/v1/imessage/messages";
const PAIRING_CODE: &str = "123456";
const TIMING: Timing = Timing {
    receive: ReceiveTiming {
        initial_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(80),
        slot_retry: Duration::from_millis(20),
    },
    poll: Duration::from_millis(10),
};

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

/// Another `inkbox` channel, holding the name while registered.
struct OccupyingChannel;

impl MessageChannel for OccupyingChannel {
    fn channel(&self) -> &str {
        inkbox::CHANNEL
    }

    fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::SendMessage)) })
    }
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

/// What the probe stores before building the channel, and what the Inkbox
/// API answers.
struct Setup {
    /// Stored configuration JSON.
    configuration: Option<&'static str>,
    mode: Option<&'static str>,
    stored: Vec<(&'static str, Vec<u8>)>,
    /// Answers in arrival order: receive polls, sign-up calls, and sends
    /// too unless `sends` routes them.
    upstream: Vec<ScriptStep>,
    /// Answers to sends, in order, on a route of their own.
    sends: Option<Vec<ScriptStep>>,
    /// Whether other receivers hold every Gateway slot.
    hold_slots: bool,
    /// Whether another `inkbox` channel is registered with the Gateway.
    occupied: bool,
}

impl Setup {
    /// A channel configured with [`CONFIG`] in `mode`, its sends routed.
    fn configured(mode: &'static str) -> Self {
        Self {
            configuration: Some(CONFIG),
            mode: Some(mode),
            sends: Some(Vec::new()),
            ..Self::unconfigured([])
        }
    }

    /// A new channel whose upstream answers `upstream` in order.
    fn unconfigured(upstream: impl IntoIterator<Item = ScriptStep>) -> Self {
        Self {
            configuration: None,
            mode: None,
            stored: Vec::new(),
            upstream: upstream.into_iter().collect(),
            sends: None,
            hold_slots: false,
            occupied: false,
        }
    }
}

/// The loopback network and the scripted Inkbox API on it, both running
/// whenever a test waits.
struct Bench {
    /// Answers every request a route does not take.
    upstream: ScriptedServer,
    /// Answers sends, when the setup routes them.
    sends: Option<ScriptedRoute>,
    driver: RefCell<Pin<Box<dyn Future<Output = ()>>>>,
}

impl Bench {
    fn new(network: LoopbackNetwork, setup: &mut Setup) -> Rc<Self> {
        let stack = network.stack();
        let upstream = ScriptedServer::new(core::mem::take(&mut setup.upstream));
        let sends = setup
            .sends
            .take()
            .map(|steps| upstream.route(SEND_ROUTE, steps));
        let serving = upstream.clone();
        let driver: Pin<Box<dyn Future<Output = ()>>> = Box::pin(async move {
            let _stopped = join(network.run(), serving.run::<4>(stack, PORT)).await;
        });
        Rc::new(Self {
            upstream,
            sends,
            driver: RefCell::new(driver),
        })
    }

    /// Runs `future` to its end while the network and the Inkbox API run.
    fn drive<F: Future>(&self, future: F) -> F::Output {
        let mut driver = self.driver.borrow_mut();
        match block_on(select(driver.as_mut(), future)) {
            Either::First(()) => panic!("the loopback network stopped"),
            Either::Second(output) => output,
        }
    }
}

/// What scenarios read from the channel, whatever its storage type.
trait Inspect {
    fn stored(&self, key: &str) -> Option<Vec<u8>>;
    fn receive_state(&self) -> ReceiveState;
    fn in_use(&self) -> usize;
    fn mode(&self) -> ChannelMode;
    fn entry_status(&self) -> EntryStatus;
    fn pairing_code(&self) -> Option<String>;
    fn owner_ids(&self) -> Vec<String>;
    fn ignored(&self) -> u32;
}

impl<Storage: PluginStorage> Inspect for InkboxState<Storage> {
    fn stored(&self, key: &str) -> Option<Vec<u8>> {
        block_on(self.storage.get_bytes(key)).expect("read storage")
    }

    fn receive_state(&self) -> ReceiveState {
        ChannelControl::receive(self).state()
    }

    fn in_use(&self) -> usize {
        ChannelControl::receive(self).in_use()
    }

    fn mode(&self) -> ChannelMode {
        ChannelControl::mode(self)
    }

    fn entry_status(&self) -> EntryStatus {
        entry_status(self.gateway.channel_status(self))
    }

    fn pairing_code(&self) -> Option<String> {
        let pairing = self.owners()?.pairing()?;
        Some(pairing.code.as_str().into())
    }

    fn owner_ids(&self) -> Vec<String> {
        self.owners()
            .expect("owners")
            .owners()
            .iter()
            .map(|owner| owner.id().into())
            .collect()
    }

    fn ignored(&self) -> u32 {
        self.owners().expect("owners").ignored()
    }
}

/// A route's answer.
struct Reply {
    status: u16,
    raw: String,
    /// `null` for an empty body.
    json: Value,
}

/// The built channel, its routes, and the scripted Inkbox API.
struct Harness {
    channel: Rc<dyn Inspect>,
    /// The receive runtime, until a test drives it or the probe spawns it.
    runtime: Rc<RefCell<Option<ReceiveRuntime>>>,
    bench: Rc<Bench>,
    webserver: Rc<WebServer>,
    gateway: Rc<IMessageGateway>,
    /// Receivers holding every Gateway slot, when the setup asks for it.
    held: RefCell<Vec<ReceiveControl<ReceiveSlots>>>,
    /// The Gateway's slot count.
    capacity: usize,
    /// Another `inkbox` channel holding the name while present.
    occupant: RefCell<Option<MessageChannelRegistration>>,
}

impl Harness {
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
            "timed out waiting for {what}: state {:?}, polls {:?}",
            self.state(),
            self.polls()
        );
    }

    /// Drives the receive runtime for `duration`.
    fn run_for(&self, duration: Duration) {
        let mut runtime = self.runtime.borrow_mut();
        let runtime = runtime.as_mut().expect("receive runtime");
        let _elapsed = self.bench.drive(select(runtime, Timer::after(duration)));
    }

    fn state(&self) -> ReceiveState {
        self.channel.receive_state()
    }

    /// Every request the default queue answered.
    fn requests(&self) -> Vec<String> {
        self.bench.upstream.requests()
    }

    /// Targets of the requests the default queue answered.
    fn polls(&self) -> Vec<String> {
        targets(self.requests())
    }

    /// Requests the send route answered.
    fn sent(&self) -> Vec<String> {
        self.bench.sends.as_ref().expect("routed sends").requests()
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
        let raw = String::from_utf8(output[split + 4..].to_vec()).expect("UTF-8 body");
        let json = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&raw).expect("JSON body")
        };
        Reply {
            status: head[9..12].parse().expect("status code"),
            raw,
            json,
        }
    }

    fn post(&self, path: &str, body: &str) -> Reply {
        self.request("POST", path, body)
    }

    /// `GET /api/gateway/inkbox/status`.
    fn get(&self) -> Reply {
        self.request("GET", &format!("{CONFIG_API_PATH}/status"), "")
    }

    /// The status JSON.
    fn status(&self) -> Value {
        let reply = self.get();
        assert_eq!(reply.status, 200);
        reply.json
    }

    fn set_mode(&self, mode: &str) -> Reply {
        let body = format!(r#"{{"mode":"{mode}"}}"#);
        self.post(&format!("{CONFIG_API_PATH}/mode"), &body)
    }

    /// The stored JSON under `key`.
    fn stored(&self, key: &str) -> Option<Value> {
        self.channel
            .stored(key)
            .map(|bytes| serde_json::from_slice(&bytes).expect("stored JSON"))
    }

    fn stored_configuration(&self) -> Option<StoredConfiguration> {
        self.channel
            .stored(CONFIGURATION_STORAGE_KEY)
            .map(|bytes| decode_configuration(&bytes).expect("stored configuration"))
    }

    /// Sends text to the `inkbox` channel through the Gateway.
    fn send_text(&self) -> bool {
        self.bench
            .drive(self.gateway.send(GatewaySendRequest {
                channel: inkbox::CHANNEL.into(),
                conversation_id: "conversation-uuid".into(),
                thread_id: None,
                reply_to: None,
                text: "hello".into(),
            }))
            .is_ok()
    }
}

/// Builds the channel the way the Plugin does, with test timing and the
/// scripted API as the signup origin, and hands it to the test.
struct Probe {
    setup: Option<Setup>,
    bench: Rc<Bench>,
    harness: Rc<RefCell<Option<Harness>>>,
    /// Whether `start` spawns the receive task the Plugin owns.
    spawn: bool,
    runtime: Rc<RefCell<Option<ReceiveRuntime>>>,
}

impl PluginDeclaration for Probe {
    const ID: &'static str = "inkbox-probe";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway", "webserver"];
}

impl Plugin for Probe {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let setup = self.setup.take().expect("setup");
        let storage = context.storage().clone();
        if let Some(configuration) = setup.configuration {
            block_on(storage.put(CONFIGURATION_STORAGE_KEY, configuration.as_bytes()))?;
        }
        if let Some(mode) = setup.mode {
            block_on(storage.put("mode", format!("\"{mode}\"").as_bytes()))?;
        }
        for (key, value) in &setup.stored {
            block_on(storage.put(key, value.as_slice()))?;
        }
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let webserver = context.require::<WebServer>("webserver")?;
        let occupant = setup.occupied.then(|| {
            let channel: Rc<dyn MessageChannel> = Rc::new(OccupyingChannel);
            gateway
                .register(channel)
                .expect("occupy the inkbox channel")
        });
        let capacity = gateway.receive_control().capacity();
        let held = if setup.hold_slots {
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
        let (state, runtime) = build_state(storage, Rc::clone(&gateway), API_BASE.into(), TIMING)?;
        context.retain(serve_routes(&state, &webserver).map_err(PluginError::registration)?);
        self.runtime.replace(Some(runtime));
        self.harness.replace(Some(Harness {
            channel: state,
            runtime: Rc::clone(&self.runtime),
            bench: Rc::clone(&self.bench),
            webserver,
            gateway,
            held: RefCell::new(held),
            capacity,
            occupant: RefCell::new(occupant),
        }));
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        if self.spawn {
            let runtime = self.runtime.take().expect("receive runtime");
            let task = inkbox_receive_task(runtime, context.task_token())
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

/// Registers the probe building the channel from `setup` on `bench`.
fn register_probe(
    manager: &mut PluginManager<MemoryPartition>,
    setup: Setup,
    bench: Rc<Bench>,
    spawn: bool,
) -> Result<Harness, barracuda_plugin::manager::PluginRegisterError> {
    let harness = Rc::new(RefCell::new(None));
    manager.register(Probe {
        setup: Some(setup),
        bench,
        harness: Rc::clone(&harness),
        spawn,
        runtime: Rc::new(RefCell::new(None)),
    })?;
    let harness = harness.take().expect("probe registered");
    Ok(harness)
}

/// Registers Workflow, HTTP, WebServer, the Gateway, and the channel from
/// `setup` on a loopback network, without starting any task, and runs
/// `scenario` against them.
fn run(mut setup: Setup, scenario: impl FnOnce(&Harness)) {
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("create test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let network = loopback_network();
    let mut context = plugin_context(&network);
    let bench = Bench::new(network, &mut setup);
    register_services(&mut manager, &mut context);
    let harness = register_probe(&mut manager, setup, bench, false).expect("register the channel");
    scenario(&harness);
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

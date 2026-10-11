//! The bench the channel's tests run on: Workflow, HTTP, WebServer, and the
//! Gateway on a loopback network, a scripted iLink service there, and the
//! channel built and served the way its Plugin does, all running on an
//! Embassy executor.
//!
//! Each test runs in a process of its own (`rusty_fork_test!`): the
//! Gateway's receive buffers and the Plugins' task pools are static.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs
)]

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use std::sync::mpsc::{sync_channel, RecvTimeoutError};

use barracuda_captive_portal_plugin::EntryStatus;
use barracuda_http_plugin::HttpPlugin;
use barracuda_imessage_gateway_plugin::{
    ChannelControl, ChannelError, ChannelFuture, IMessageGateway, IMessageGatewayPlugin,
    MessageChannel, Operation, ReceiveControl, ReceiveRuntime, ReceiveSlots, ReceiveState,
    SendMessageRequest, SendReceipt,
};
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_platform_test::{
    install_global_memory_vfs, loopback_network, memory_partition, MemoryPartition, ScriptStep,
    ScriptedRoute, ScriptedServer,
};
use barracuda_plugin::api::{PluginContext, SharedEntropy};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginError, PluginManager, PluginRegisterContext, PluginResult,
    PluginStartContext, PluginStorage,
};
use barracuda_webserver_plugin::{WebServer, WebServerPlugin};
use barracuda_workflow_plugin::{
    workflow_action_schema_inline, WorkflowActionFuture, WorkflowActionHandler,
    WorkflowActionRegistry, WorkflowActionSchema, WorkflowPlugin, WorkflowService,
};
use embassy_executor::{Executor, Spawner};
use embassy_futures::join::join;
use embassy_time::{Duration, Instant, Timer};
use futures_lite::future::{block_on, yield_now};
use portable_atomic::{AtomicU32, Ordering};
use portable_atomic_util::Arc;
use serde_json::{json, Value};

use crate::login::{LoginRuntime, LoginWatch};
use crate::receive::{BOT_ID_STORAGE_KEY, CURSOR_STORAGE_KEY};
use crate::{
    build_channel, entry_status, serve_routes, wechat_login_task, wechat_receive_task,
    ChannelConfiguration, ConfigRequest, Timing, CONFIGURATION_STORAGE_KEY, CONFIG_API_PATH,
    LOGIN_API_PATH,
};

/// Port of the scripted iLink service on the loopback network.
const PORT: u16 = 9000;
/// Where the scripted iLink service answers.
pub(crate) const API_BASE: &str = "http://10.0.0.1:9000";
/// A port on the loopback network nobody listens on.
pub(crate) const UNREACHABLE_API_BASE: &str = "http://10.0.0.1:9001";
/// The code the Gateway mints from the first draw of [`Counter`].
pub(crate) const PAIRING_CODE: &str = "000007";

/// Test entropy: a counter, so pairing codes and UINs differ per draw.
#[derive(Clone)]
struct Counter(Arc<AtomicU32>);

impl Entropy for Counter {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        let value = self.0.fetch_add(1, Ordering::Relaxed).wrapping_add(7);
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = value.to_le_bytes()[index % 4];
        }
        Ok(())
    }
}

fn counter() -> SharedEntropy {
    SharedEntropy::new(Counter(Arc::new(AtomicU32::new(0))))
}

/// What the store holds before the channel loads.
#[derive(Default)]
pub(crate) struct Stored {
    pub(crate) configured: bool,
    pub(crate) mode: Option<&'static str>,
    pub(crate) owners: Vec<&'static str>,
    pub(crate) cursor: Option<&'static str>,
    pub(crate) bot_id: Option<&'static str>,
}

impl Stored {
    /// A bot linked by QR login with the token `bot-token`.
    pub(crate) fn linked() -> Self {
        Self {
            configured: true,
            ..Self::default()
        }
    }

    pub(crate) fn with_owner(mut self, owner: &'static str) -> Self {
        self.owners.push(owner);
        self
    }
}

/// Options of one test.
pub(crate) struct Setup {
    pub(crate) stored: Stored,
    /// Answers of the default queue: `getupdates` and the login calls.
    pub(crate) steps: Vec<ScriptStep>,
    /// Answers to `sendmessage`.
    pub(crate) sends: Vec<ScriptStep>,
    /// Answers to CDN downloads (`/c2c/download`).
    pub(crate) downloads: Vec<ScriptStep>,
    /// Other receivers hold every Gateway receive slot before the channel
    /// loads.
    pub(crate) hold_slots: bool,
    /// Another `wechat` channel is registered with the Gateway.
    pub(crate) occupy_channel: bool,
    pub(crate) timing: Timing,
    /// Where a QR login goes while no configuration is stored.
    pub(crate) login_api_base: &'static str,
}

impl Setup {
    pub(crate) fn new(stored: Stored, steps: Vec<ScriptStep>, timing: Timing) -> Self {
        Self {
            stored,
            steps,
            sends: Vec::new(),
            downloads: Vec::new(),
            hold_slots: false,
            occupy_channel: false,
            timing,
            login_api_base: API_BASE,
        }
    }
}

/// A test body; it runs once every Plugin, the channel's runtimes among
/// them, has started.
pub(crate) trait Scenario: Send + 'static {
    async fn run<S: PluginStorage>(self, harness: Rc<Harness<S>>);
}

/// Declares a [`Scenario`] named `$name` whose body sees `$harness`.
macro_rules! scenario {
    ($name:ident, |$harness:ident| $body:block) => {
        struct $name;

        impl $crate::bench::Scenario for $name {
            async fn run<S: barracuda_plugin::manager::PluginStorage>(
                self,
                $harness: alloc::rc::Rc<$crate::bench::Harness<S>>,
            ) $body
        }
    };
}
pub(crate) use scenario;

/// A second `wechat` channel, used to occupy the name or to probe the
/// Gateway registration.
struct OtherWechatChannel;

impl MessageChannel for OtherWechatChannel {
    fn channel(&self) -> &str {
        "wechat"
    }

    fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::SendMessage)) })
    }
}

/// The running channel, its routes on the WebServer, and the scripted iLink
/// service.
pub(crate) struct Harness<S> {
    pub(crate) channel: Rc<ChannelConfiguration<S>>,
    login: LoginWatch,
    gateway: Rc<IMessageGateway>,
    webserver: Rc<WebServer>,
    /// Answers `getupdates` and the login calls.
    pub(crate) server: ScriptedServer,
    /// Answers `sendmessage`.
    sends: ScriptedRoute,
    /// Answers CDN downloads.
    pub(crate) downloads: ScriptedRoute,
    /// Receivers holding every Gateway slot, when the setup asks for it.
    held: RefCell<Vec<ReceiveControl<ReceiveSlots>>>,
    /// The Gateway's slot count.
    pub(crate) capacity: usize,
    /// Every `gateway.message.received` a Workflow handed the recorder.
    records: Rc<RefCell<Vec<Value>>>,
    storage: S,
}

impl<S: PluginStorage> Harness<S> {
    /// Waits until `condition` holds; fails after 5 s.
    pub(crate) async fn until(&self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; state {:?}, requests {:?}",
                self.state(),
                self.request_paths()
            );
            Timer::after(Duration::from_millis(1)).await;
        }
    }

    /// Lets every task run for a while.
    pub(crate) async fn settle(&self) {
        for _ in 0..200 {
            yield_now().await;
        }
        Timer::after(Duration::from_millis(50)).await;
    }

    /// `(conversation, message id, text)` of every published message, in
    /// order, as Workflow saw them.
    pub(crate) fn published(&self) -> Vec<(String, String, String)> {
        self.records
            .borrow()
            .iter()
            .map(|record| {
                assert_eq!(record["route"]["channel"], "wechat");
                assert_eq!(record["route"]["thread_id"], Value::Null);
                let text = |value: &Value| value.as_str().expect("a string").to_string();
                (
                    text(&record["route"]["conversation_id"]),
                    text(&record["message_id"]),
                    text(&record["text"]),
                )
            })
            .collect()
    }

    /// The `attachments` of every published message, in order; `null` for
    /// one without.
    pub(crate) fn attachments(&self) -> Vec<Value> {
        self.records
            .borrow()
            .iter()
            .map(|record| record["attachments"].clone())
            .collect()
    }

    /// Requests of the default queue: `getupdates` and the login calls.
    pub(crate) fn requests(&self) -> Vec<Request> {
        self.server
            .requests()
            .into_iter()
            .map(Request::parse)
            .collect()
    }

    pub(crate) fn request_paths(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|request| request.path)
            .collect()
    }

    /// JSON bodies of the `getupdates` polls.
    pub(crate) fn poll_bodies(&self) -> Vec<Value> {
        self.requests().iter().map(Request::json).collect()
    }

    pub(crate) fn send_requests(&self) -> Vec<Request> {
        self.sends
            .requests()
            .into_iter()
            .map(Request::parse)
            .collect()
    }

    pub(crate) async fn stored(&self, key: &'static str) -> Option<String> {
        self.storage
            .get_bytes(key)
            .await
            .expect("read storage")
            .map(|bytes| String::from_utf8(bytes).expect("UTF-8 value"))
    }

    /// Whether a `wechat` channel is registered with the Gateway.
    pub(crate) fn registered(&self) -> bool {
        self.gateway.register(Rc::new(OtherWechatChannel)).is_err()
    }

    pub(crate) fn state(&self) -> ReceiveState {
        self.channel.receive.state()
    }

    /// Receive slots held across every channel.
    pub(crate) fn in_use(&self) -> usize {
        self.channel.receive.in_use()
    }

    /// Frees the slots the setup held.
    pub(crate) fn release_slots(&self) {
        self.held.take();
    }

    /// The channel's portal card.
    pub(crate) fn entry_status(&self) -> EntryStatus {
        entry_status(&self.channel, &self.login)
    }

    /// Requests `path` on the WebServer; an empty answer is `null`.
    pub(crate) async fn request(&self, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
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
        let body = &output[split + 4..];
        let json = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(body).expect("JSON response")
        };
        (head[9..12].parse().expect("status code"), json)
    }

    /// The configuration path.
    pub(crate) async fn config(&self, method: &str, body: &[u8]) -> (u16, Value) {
        self.request(method, CONFIG_API_PATH, body).await
    }

    /// `GET /api/gateway/wechat/status`.
    pub(crate) async fn status(&self) -> (u16, Value) {
        self.request("GET", &format!("{CONFIG_API_PATH}/status"), b"")
            .await
    }

    /// `POST /api/gateway/wechat/mode`.
    pub(crate) async fn set_mode(&self, body: &[u8]) -> (u16, Value) {
        self.request("POST", &format!("{CONFIG_API_PATH}/mode"), body)
            .await
    }

    /// `/api/gateway/wechat/owners`.
    pub(crate) async fn owners(&self, method: &str, body: &[u8]) -> (u16, Value) {
        self.request(method, &format!("{CONFIG_API_PATH}/owners"), body)
            .await
    }

    /// `/api/gateway/wechat/login`.
    pub(crate) async fn login(&self, method: &str, body: &[u8]) -> (u16, Value) {
        self.request(method, LOGIN_API_PATH, body).await
    }
}

/// One request the scripted iLink service answered.
pub(crate) struct Request {
    pub(crate) path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Request {
    fn parse(raw: String) -> Self {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let mut lines = head.split("\r\n");
        let path = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or_default()
            .into();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.into(), value.trim().into()))
            .collect();
        Self {
            path,
            headers,
            body: body.into(),
        }
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("JSON request")
    }
}

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

/// Loads a Workflow that hands every inbound message to [`Record`].
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

type TestFuture = Pin<Box<dyn Future<Output = ()>>>;

/// Builds and serves the channel the way its Plugin does, with the setup's
/// timing, and hands its runtimes to the Plugin's own tasks.
struct Probe<Sc> {
    setup: Option<Setup>,
    scenario: Option<Sc>,
    server: ScriptedServer,
    sends: ScriptedRoute,
    downloads: ScriptedRoute,
    records: Rc<RefCell<Vec<Value>>>,
    test: Rc<RefCell<Option<TestFuture>>>,
    runtimes: Option<(ReceiveRuntime, LoginRuntime)>,
}

impl<Sc> PluginDeclaration for Probe<Sc> {
    const ID: &'static str = "wechat-probe";
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
        let setup = self.setup.take().expect("one registration");
        let scenario = self.scenario.take().expect("one registration");
        let storage = context.storage().clone();
        block_on(seed(&storage, &setup.stored));
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let webserver = context.require::<WebServer>("webserver")?;
        if setup.occupy_channel {
            context.retain(
                gateway
                    .register(Rc::new(OtherWechatChannel))
                    .map_err(PluginError::registration)?,
            );
        }
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
        let built = build_channel(
            storage.clone(),
            Rc::clone(&gateway),
            counter(),
            setup.login_api_base.into(),
            setup.timing,
        )?;
        // The first entropy draw mints the code, so it is known in advance. An
        // unlinked channel has no owner book yet.
        if let Some(owners) = built.channel.owners() {
            let code = owners.pairing().map(|pairing| pairing.code);
            assert_eq!(code.as_ref().map(|code| code.as_str()), Some(PAIRING_CODE));
        }
        let login = built.login.watch();
        for route in serve_routes(&gateway, &webserver, &built.channel, built.login)
            .map_err(PluginError::registration)?
        {
            context.retain(route);
        }
        let harness = Rc::new(Harness {
            channel: built.channel,
            login,
            gateway,
            webserver,
            server: self.server.clone(),
            sends: self.sends.clone(),
            downloads: self.downloads.clone(),
            held: RefCell::new(held),
            capacity,
            records: Rc::clone(&self.records),
            storage,
        });
        self.test.replace(Some(Box::pin(scenario.run(harness))));
        self.runtimes = Some((built.receive_runtime, built.login_runtime));
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let (receive, login) = self.runtimes.take().expect("one start");
        let receive = wechat_receive_task(receive, context.task_token())
            .map_err(PluginError::registration)?;
        let login =
            wechat_login_task(login, context.task_token()).map_err(PluginError::registration)?;
        let spawner = context.task_spawner()?;
        spawner.spawn(receive);
        spawner.spawn(login);
        Ok(())
    }
}

async fn seed<S: PluginStorage>(storage: &S, stored: &Stored) {
    if stored.configured {
        let mut config = ConfigRequest::from_login("bot-token".into(), API_BASE.into());
        config.cdn_base = format!("{API_BASE}/c2c");
        let bytes = serde_json::to_vec(&config).expect("encode configuration");
        storage
            .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
            .await
            .expect("store configuration");
    }
    if let Some(mode) = stored.mode {
        storage
            .put("mode", format!("\"{mode}\"").as_bytes())
            .await
            .expect("store mode");
    }
    if !stored.owners.is_empty() {
        let owners: Vec<Value> = stored
            .owners
            .iter()
            .map(|id| json!({ "id": id, "label": null }))
            .collect();
        storage
            .put("owners", Value::from(owners).to_string().as_bytes())
            .await
            .expect("store owners");
    }
    if let Some(cursor) = stored.cursor {
        storage
            .put(CURSOR_STORAGE_KEY, cursor.as_bytes())
            .await
            .expect("store cursor");
    }
    if let Some(bot_id) = stored.bot_id {
        storage
            .put(BOT_ID_STORAGE_KEY, bot_id.as_bytes())
            .await
            .expect("store bot id");
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
    context.entropy = counter();
    manager
        .register(IMessageGatewayPlugin::new(context))
        .expect("register IMessage Gateway Plugin");
}

/// Starts Workflow, HTTP, WebServer, the Gateway, a Workflow recording
/// inbound messages, and the channel on a loopback network, then runs
/// `scenario`.
async fn bench(spawner: Spawner, mut setup: Setup, scenario: impl Scenario) {
    install_global_memory_vfs().await.expect("install test VFS");
    let partition = memory_partition(64 * 1024)
        .await
        .expect("create test database region");
    let mut manager = PluginManager::open(partition)
        .await
        .expect("open Plugin storage");
    manager.install_vfs(barracuda_vfs::global_namespace().await);
    manager.install_task_spawner(spawner);
    let network = loopback_network();
    let stack = network.stack();
    let mut context = plugin_context(&network);
    let server = ScriptedServer::new(core::mem::take(&mut setup.steps));
    let sends = server.route("/ilink/bot/sendmessage", core::mem::take(&mut setup.sends));
    let downloads = server.route("/c2c/download", core::mem::take(&mut setup.downloads));
    let serving = server.clone();
    spawner.spawn(
        run_future(Box::pin(async move {
            let _stopped = join(network.run(), serving.run::<6>(stack, PORT)).await;
        }))
        .expect("spawn the network"),
    );
    register_services(&mut manager, &mut context);
    let records = Rc::new(RefCell::new(Vec::new()));
    manager
        .register(Recorder(Rc::clone(&records)))
        .expect("register the inbound recorder");
    let test = Rc::new(RefCell::new(None));
    manager
        .register(Probe {
            setup: Some(setup),
            scenario: Some(scenario),
            server,
            sends,
            downloads,
            records,
            test: Rc::clone(&test),
            runtimes: None,
        })
        .expect("register the WeChat channel");
    manager.start().expect("start the Plugins");
    let test = test.take().expect("the probe built the test");
    test.await;
    // The executor thread outlives the test; keep its Plugins running.
    core::mem::forget(manager);
}

#[embassy_executor::task(pool_size = 2)]
async fn run_future(future: TestFuture) {
    future.await;
}

/// Runs `scenario` on the bench in an executor thread; a panic there fails
/// the test.
pub(crate) fn run(setup: Setup, scenario: impl Scenario) {
    let (done, finished) = sync_channel(1);
    let executor_thread = std::thread::Builder::new()
        .name("wechat-bench".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(move |spawner| {
                let test: TestFuture = Box::pin(async move {
                    bench(spawner, setup, scenario).await;
                    let _sent = done.send(());
                });
                spawner.spawn(run_future(test).expect("spawn the test"));
            });
        })
        .expect("spawn the executor thread");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match finished.recv_timeout(std::time::Duration::from_millis(10)) {
            Ok(()) => return,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        if executor_thread.is_finished() {
            match executor_thread.join() {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(()) => panic!("the executor stopped"),
            }
        }
        assert!(std::time::Instant::now() < deadline, "the test timed out");
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

//! Web chat files on the real Gateway, WebServer and Workflow: uploads,
//! message frames that send them, media messages the channel sends, and the
//! download route, all as real HTTP and WebSocket exchanges.
//!
//! Every test runs in a process of its own: the memory filesystem is
//! process-wide and the Plugins' tasks come from static pools.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    missing_docs
)]
#![recursion_limit = "256"]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::task::{Poll, Waker};
use std::time::Duration as StdDuration;

use barracuda_captive_portal_plugin::CaptivePortalPlugin;
use barracuda_http_plugin::HttpPlugin;
use barracuda_imessage_gateway_plugin::{
    GatewaySendMediaRequest, GatewaySendMediaResponse, IMessageGateway, IMessageGatewayPlugin,
    MediaKind,
};
use barracuda_imessage_web_plugin::IMessageWebPlugin;
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin::api::{BoardInfo, Hardware, PlatformInfo, PluginContext, TargetIdentity};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginError, PluginFilesystem, PluginManager, PluginRegisterContext,
    PluginRequirements, PluginResult, PluginStorage,
};
use barracuda_vfs::ScopedVfs;
use barracuda_webserver_plugin::{WebServer, WebServerPlugin};
use barracuda_workflow_plugin::{
    workflow_action_schema_inline, WorkflowActionFuture, WorkflowActionHandler,
    WorkflowActionRegistry, WorkflowActionSchema, WorkflowPlugin, WorkflowService,
};
use embassy_executor::{Executor, Spawner};
use embassy_futures::select::{select, Either};
use embassy_time::Timer;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use picoserve::io::Socket;
use picoserve::time::{Duration, TimeoutError, Timer as PicoTimer};
use rusty_fork::rusty_fork_test;
use serde_json::{json, Value};

const LIMIT: usize = 8 * 1024 * 1024;

// --- Bench ------------------------------------------------------------------

/// The Plugins under test, seen from another Plugin.
struct Bench {
    server: Rc<WebServer>,
    gateway: Rc<IMessageGateway>,
    filesystem: ScopedVfs,
    /// Every `gateway.message.received` a Workflow handed the recorder.
    records: Rc<RefCell<Vec<Value>>>,
}

type Captured = Rc<RefCell<Option<(Rc<WebServer>, Rc<IMessageGateway>, ScopedVfs)>>>;

struct Probe(Captured);

impl PluginDeclaration for Probe {
    const ID: &'static str = "web-probe";
    const DEPENDS_ON: &'static [&'static str] = &["webserver", "imessage-gateway", "imessage-web"];
}

impl Plugin for Probe {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage: PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        let server = context.require::<WebServer>("webserver")?;
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        self.0
            .replace(Some((server, gateway, context.filesystem()?.clone())));
        Ok(())
    }
}

/// Records every message a Workflow hands it.
struct Record(Rc<RefCell<Vec<Value>>>);

impl WorkflowActionHandler for Record {
    type Request = Value;
    type Response = Value;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
        "test.record",
        r#"{"type":"object"}"#,
        r#"{"type":"object"}"#
    );

    fn invoke(&self, request: Value) -> WorkflowActionFuture<'_, Value> {
        self.0.borrow_mut().push(request);
        Box::pin(async { Ok(json!({})) })
    }
}

struct Recorder(Rc<RefCell<Vec<Value>>>);

impl PluginDeclaration for Recorder {
    const ID: &'static str = "inbound-recorder";
    const DEPENDS_ON: &'static [&'static str] = &["workflow"];
}

impl Plugin for Recorder {
    fn register<Storage: PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let service = context.require::<WorkflowService>("workflow")?;
        context.retain(
            actions
                .add_action(Record(Rc::clone(&self.0)))
                .map_err(PluginError::registration)?,
        );
        embassy_futures::block_on(service.load_transient(
            r#"{"id":"record","match":{"event":"gateway.message.received"},"steps":[{"call":"test.record"}]}"#,
        ))
        .map_err(PluginError::registration)?;
        Ok(())
    }
}

type Scenario = fn(Rc<Bench>) -> Pin<Box<dyn Future<Output = ()>>>;

/// Runs `scenario` on an executor of its own, beside started Workflow, HTTP,
/// WebServer, Gateway, portal and Web chat Plugins.
fn run(scenario: Scenario) {
    let (done, finished) = sync_channel(1);
    std::thread::Builder::new()
        .name("web-scenario".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner.spawn(scenario_task(spawner, scenario, done).expect("spawn the scenario"));
            });
        })
        .expect("spawn the executor thread");
    finished
        .recv_timeout(StdDuration::from_secs(60))
        .expect("the scenario failed or timed out");
}

#[embassy_executor::task]
async fn scenario_task(spawner: Spawner, scenario: Scenario, done: SyncSender<()>) {
    install_global_memory_vfs().await.expect("install test VFS");
    let partition = memory_partition(64 * 1024)
        .await
        .expect("test database region");
    let mut manager = PluginManager::open(partition)
        .await
        .expect("open Plugin storage");
    manager.install_vfs(barracuda_vfs::global_namespace().await);
    manager.install_task_spawner(spawner);
    let identity = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    let mut context = PluginContext::new(identity, never_embassy_stack());
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
        .register(CaptivePortalPlugin::new(&mut context))
        .expect("register portal");
    manager
        .register(IMessageWebPlugin::new(&mut context))
        .expect("register Web chat");
    let records = Rc::new(RefCell::new(Vec::new()));
    manager
        .register(Recorder(Rc::clone(&records)))
        .expect("register recorder");
    let captured: Captured = Rc::new(RefCell::new(None));
    manager
        .register(Probe(Rc::clone(&captured)))
        .expect("register probe");
    manager.start().expect("start the Plugins");
    let (server, gateway, filesystem) = captured.take().expect("the probe ran");
    scenario(Rc::new(Bench {
        server,
        gateway,
        filesystem,
        records,
    }))
    .await;
    done.send(()).expect("report the outcome");
    // the executor thread outlives the test; keep its Plugins alive
    core::mem::forget(manager);
}

/// Waits until `ready` holds, failing after a few seconds.
async fn until(what: &str, ready: impl Fn() -> bool) {
    for _ in 0..3000 {
        if ready() {
            return;
        }
        Timer::after_millis(1).await;
    }
    panic!("timed out waiting for {what}");
}

// --- HTTP -------------------------------------------------------------------

struct Response {
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

impl Response {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }
}

async fn request(server: &WebServer, method: &str, path: &str, body: &[u8]) -> Response {
    let mut input = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    input.extend_from_slice(body);
    let wire = Wire::new(input);
    server
        .serve_connection(TestTimer, &mut [0; 4096], wire.socket(true))
        .await
        .expect("serve");
    let bytes = wire.0.borrow().output.clone();
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("headers");
    let head = std::str::from_utf8(&bytes[..split]).expect("ASCII headers");
    Response {
        status: head[9..12].parse().expect("status"),
        content_type: head
            .lines()
            .find_map(|line| line.strip_prefix("Content-Type: "))
            .unwrap_or_default()
            .to_owned(),
        body: bytes[split + 4..].to_vec(),
    }
}

async fn upload(bench: &Bench, mime: &str, name: &str, body: &[u8]) -> Response {
    let path = format!(
        "/api/web/upload/{}/{}",
        mime.replace('/', "%2F"),
        name.replace(' ', "%20")
    );
    request(&bench.server, "PUT", &path, body).await
}

/// The id of a successful upload.
async fn uploaded(bench: &Bench, mime: &str, name: &str, body: &[u8]) -> String {
    let response = upload(bench, mime, name, body).await;
    assert_eq!(
        response.status,
        201,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    response.json()["id"].as_str().expect("an id").to_owned()
}

fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|index| b'a' + (index % 26) as u8).collect()
}

// --- WebSocket --------------------------------------------------------------

#[derive(Default)]
struct Shared {
    input: VecDeque<u8>,
    /// Whether the input ends once drained, or waits for more.
    ends: bool,
    waker: Option<Waker>,
    output: Vec<u8>,
}

/// Both directions of one test connection.
#[derive(Clone)]
struct Wire(Rc<RefCell<Shared>>);

impl Wire {
    fn new(input: Vec<u8>) -> Self {
        Self(Rc::new(RefCell::new(Shared {
            input: input.into(),
            ..Shared::default()
        })))
    }

    fn socket(&self, ends: bool) -> TestSocket {
        self.0.borrow_mut().ends = ends;
        TestSocket {
            input: Input(self.clone()),
            output: Output(self.clone()),
        }
    }

    /// Sends one text frame, masked with a zero key, as a browser would.
    fn send(&self, text: &str) {
        let mut shared = self.0.borrow_mut();
        let length = text.len();
        shared.input.push_back(0x81);
        if length < 126 {
            shared.input.push_back(0x80 | length as u8);
        } else {
            shared.input.push_back(0x80 | 126);
            shared.input.extend((length as u16).to_be_bytes());
        }
        shared.input.extend([0, 0, 0, 0]);
        shared.input.extend(text.bytes());
        if let Some(waker) = shared.waker.take() {
            waker.wake();
        }
    }

    /// Every text frame the server sent after its upgrade, parsed as SSE.
    fn events(&self) -> Vec<(String, Value)> {
        let output = self.0.borrow().output.clone();
        let Some(split) = output.windows(4).position(|window| window == b"\r\n\r\n") else {
            return Vec::new();
        };
        let mut frames = &output[split + 4..];
        let mut events = Vec::new();
        while frames.len() >= 2 {
            let (length, start) = match frames[1] & 0x7f {
                126 if frames.len() >= 4 => {
                    (u16::from_be_bytes([frames[2], frames[3]]) as usize, 4)
                }
                127 if frames.len() >= 10 => (
                    u64::from_be_bytes(frames[2..10].try_into().unwrap()) as usize,
                    10,
                ),
                short if short < 126 => (short as usize, 2),
                _ => break,
            };
            if frames.len() < start + length {
                break;
            }
            let text = std::str::from_utf8(&frames[start..start + length]).expect("UTF-8");
            let event = text
                .lines()
                .find_map(|line| line.strip_prefix("event: "))
                .unwrap_or_default()
                .to_owned();
            let data = text
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .map(|data| serde_json::from_str(data).expect("JSON data"))
                .unwrap_or(Value::Null);
            events.push((event, data));
            frames = &frames[start + length..];
        }
        events
    }

    fn upgraded(&self) -> bool {
        self.0.borrow().output.starts_with(b"HTTP/1.1 101")
    }
}

/// Runs `steps` beside a WebSocket client connected to `/ws/message`.
async fn chat<F: Future<Output = ()>>(bench: &Bench, steps: impl FnOnce(Wire) -> F) {
    let wire = Wire::new(
        b"GET /ws/message HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n".to_vec(),
    );
    let mut buffer = [0; 4096];
    let connection = bench
        .server
        .serve_connection(TestTimer, &mut buffer, wire.socket(false));
    let client = wire.clone();
    let steps = async move {
        until("the upgrade", || client.upgraded()).await;
        // the bridge subscribes once its callback runs
        Timer::after_millis(20).await;
        steps(client).await;
    };
    match select(connection, steps).await {
        Either::First(outcome) => panic!("the connection ended: {:?}", outcome.err()),
        Either::Second(()) => {}
    }
}

/// The events named `name` the client got so far.
fn named(wire: &Wire, name: &str) -> Vec<Value> {
    wire.events()
        .into_iter()
        .filter(|(event, _data)| event == name)
        .map(|(_event, data)| data)
        .collect()
}

struct Input(Wire);

impl ErrorType for Input {
    type Error = ErrorKind;
}

impl Read for Input {
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, ErrorKind> {
        poll_fn(|context| {
            let mut shared = self.0 .0.borrow_mut();
            if shared.input.is_empty() {
                if shared.ends {
                    return Poll::Ready(Ok(0));
                }
                shared.waker = Some(context.waker().clone());
                return Poll::Pending;
            }
            let count = bytes.len().min(shared.input.len());
            for (target, byte) in bytes.iter_mut().zip(shared.input.drain(..count)) {
                *target = byte;
            }
            Poll::Ready(Ok(count))
        })
        .await
    }
}

struct Output(Wire);

impl ErrorType for Output {
    type Error = ErrorKind;
}

impl Write for Output {
    async fn write(&mut self, bytes: &[u8]) -> Result<usize, ErrorKind> {
        self.0 .0.borrow_mut().output.extend_from_slice(bytes);
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
        self.0
             .0
            .borrow_mut()
            .output
            .extend_from_slice(&scratch[..count]);
        Ok(result)
    }
}

struct TestSocket {
    input: Input,
    output: Output,
}

impl Socket<()> for TestSocket {
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
        (&mut self.input, &mut self.output)
    }

    async fn abort<T: PicoTimer<()>>(
        self,
        _: &picoserve::Timeouts,
        _: &mut T,
    ) -> Result<(), picoserve::Error<ErrorKind>> {
        Ok(())
    }

    async fn shutdown<T: PicoTimer<()>>(
        self,
        _: &picoserve::Timeouts,
        _: &mut T,
    ) -> Result<(), picoserve::Error<ErrorKind>> {
        Ok(())
    }
}

struct TestTimer;

impl PicoTimer<()> for TestTimer {
    async fn delay(&self, _: Duration) {
        core::future::pending().await
    }

    async fn run_with_timeout<F: Future>(
        &self,
        _: Duration,
        future: F,
    ) -> Result<F::Output, TimeoutError> {
        Ok(future.await)
    }
}

// --- Scenarios --------------------------------------------------------------

/// An upload streams into the Gateway's storage and is answered with an id.
fn stores_an_upload(bench: Rc<Bench>) -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(async move {
        // four times the HTTP buffer, so it streams
        let body = pattern(20_000);
        let response = upload(&bench, "image/PNG", "cat photo.png", &body).await;
        assert_eq!(response.status, 201);
        assert_eq!(response.content_type, "application/json");
        let answer = response.json();
        let id = answer["id"].as_str().expect("an id");
        assert_eq!(
            answer,
            json!({"id": id, "kind": "image", "mime_type": "image/png",
                   "filename": "cat photo.png", "size": 20_000})
        );
        let names = bench
            .filesystem
            .list_dir("/workspace/cache/gateway/web")
            .await
            .expect("the Gateway's web directory");
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(names[0].ends_with(".png"), "{names:?}");
        let stored = bench
            .filesystem
            .read(&format!("/workspace/cache/gateway/web/{}", names[0]))
            .await
            .expect("the stored file");
        assert_eq!(stored, body);

        let pdf = upload(&bench, "application/pdf", "notes.pdf", b"%PDF-1.7").await;
        assert_eq!(pdf.status, 201);
        assert_eq!(pdf.json()["kind"], "file");
        let clip = upload(&bench, "video/mp4", "clip.mp4", b"....ftyp").await;
        assert_eq!(clip.json()["kind"], "video");
        let svg = upload(&bench, "image/svg+xml", "logo.svg", b"<svg/>").await;
        assert_eq!(svg.json()["kind"], "file", "only safe pictures are images");
    })
}

/// A refused upload says why and stores nothing.
fn refuses_bad_uploads(bench: Rc<Bench>) -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(async move {
        assert_eq!(bench.gateway.attachment_max_bytes(), LIMIT as u64);
        let large = upload(&bench, "image/png", "big.png", &vec![0; LIMIT + 1]).await;
        assert_eq!(large.status, 413);
        assert_eq!(large.json(), json!({"error": "too_large", "limit": LIMIT}));

        for path in [
            "/api/web/upload/notamime/a.png",
            "/api/web/upload/image%2Fpng",
            "/api/web/upload/image%2Fpng/",
            "/api/web/upload/image%2Fpng/a%2Fb.png",
            "/api/web/upload/image%2Fpng%3Bq%3D1/a.png",
            "/api/web/upload/image%2Fpng/%E2%9C",
        ] {
            let refused = request(&bench.server, "PUT", path, b"x").await;
            assert_eq!(refused.status, 400, "{path}");
            assert_eq!(refused.json(), json!({"error": "invalid_path"}));
        }
        let empty = request(
            &bench.server,
            "PUT",
            "/api/web/upload/image%2Fpng/a.png",
            b"",
        )
        .await;
        assert_eq!(empty.status, 411);
        assert_eq!(empty.json(), json!({"error": "length_required"}));
        let get = request(
            &bench.server,
            "GET",
            "/api/web/upload/image%2Fpng/a.png",
            b"",
        )
        .await;
        assert_eq!(get.status, 405);
        assert!(bench
            .filesystem
            .list_dir("/workspace/cache/gateway/web")
            .await
            .map_or(true, |names| names.is_empty()));
    })
}

/// A message frame sends the uploads it names with its text, once.
fn sends_uploads_with_a_message(bench: Rc<Bench>) -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(async move {
        let picture = uploaded(&bench, "image/jpeg", "beach.jpg", &pattern(3000)).await;
        let notes = uploaded(&bench, "application/pdf", "notes.pdf", b"%PDF").await;
        let later = uploaded(&bench, "audio/webm", "memo.webm", b"OggS").await;
        let records = Rc::clone(&bench.records);
        chat(&bench, |wire| async move {
            wire.send(&format!(
                r#"{{"text":"look","attachments":["{picture}","{notes}"]}}"#
            ));
            until("the first message", || records.borrow().len() == 1).await;
            let message = records.borrow()[0].clone();
            assert_eq!(message["message_id"], "web-in-1");
            assert_eq!(message["route"]["channel"], "web");
            assert_eq!(message["route"]["conversation_id"], "conversation");
            assert_eq!(message["text"], "look");
            let attachments = message["attachments"].as_array().expect("attachments");
            assert_eq!(attachments.len(), 2);
            assert_eq!(attachments[0]["kind"], "image");
            assert_eq!(attachments[0]["mime_type"], "image/jpeg");
            assert_eq!(attachments[0]["filename"], "beach.jpg");
            assert_eq!(attachments[0]["size"], 3000);
            let path = attachments[0]["path"].as_str().expect("a path");
            assert!(path.starts_with("/workspace/cache/gateway/web/") && path.ends_with(".jpg"));
            assert_eq!(attachments[1]["kind"], "file");
            assert_eq!(attachments[1]["filename"], "notes.pdf");

            // an id is sent once; a frame naming a spent one is refused whole
            wire.send(&format!(
                r#"{{"text":"again","attachments":["{later}","{picture}"]}}"#
            ));
            until("the refusal", || {
                !named(&wire, "message.rejected").is_empty()
            })
            .await;
            assert_eq!(
                named(&wire, "message.rejected"),
                [json!({"error": "unknown_attachment", "message_id": "web-in-2"})]
            );
            // files alone need no text, and a refused frame consumed nothing
            wire.send(&format!(r#"{{"text":"","attachments":["{later}"]}}"#));
            until("the second message", || records.borrow().len() == 2).await;
            let message = records.borrow()[1].clone();
            assert_eq!(message["message_id"], "web-in-3");
            assert_eq!(message["text"], "");
            assert_eq!(message["attachments"][0]["kind"], "audio");

            let five = format!(r#""{later}","#).repeat(5);
            wire.send(&format!(
                r#"{{"text":"x","attachments":[{}]}}"#,
                five.trim_end_matches(',')
            ));
            wire.send(r#"{"text":""}"#);
            wire.send(r#"{"text":"plain"}"#);
            until("the third message", || records.borrow().len() == 3).await;
            assert_eq!(records.borrow()[2]["message_id"], "web-in-6");
            assert_eq!(records.borrow()[2]["text"], "plain");
            assert!(records.borrow()[2].get("attachments").is_none());
            assert_eq!(
                named(&wire, "message.rejected")[1..],
                [
                    json!({"error": "too_many_attachments", "message_id": "web-in-4"}),
                    json!({"error": "empty", "message_id": "web-in-5"}),
                ]
            );
        })
        .await;
    })
}

fn file(kind: MediaKind, path: &str, mime_type: Option<&str>) -> GatewaySendMediaRequest {
    GatewaySendMediaRequest::File {
        channel: "web".into(),
        conversation_id: "conversation".into(),
        thread_id: None,
        kind,
        path: path.into(),
        filename: None,
        mime_type: mime_type.map(String::from),
        caption: None,
        reply_to: None,
    }
}

/// Media the channel sends is one event naming a URL, served by its type.
fn serves_sent_media(bench: Rc<Bench>) -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(async move {
        let clip = pattern(5000);
        bench
            .filesystem
            .write("/workspace/media/clip.mp4", &clip)
            .await
            .expect("a clip");
        bench
            .filesystem
            .write("/workspace/media/page.html", b"<script>alert(1)</script>")
            .await
            .expect("a page");
        bench
            .filesystem
            .write("/workspace/media/logo.svg", b"<svg onload=alert(1)/>")
            .await
            .expect("a picture");
        let gateway = Rc::clone(&bench.gateway);
        let server = Rc::clone(&bench.server);
        let filesystem = bench.filesystem.clone();
        chat(&bench, |wire| async move {
            for request in [
                file(MediaKind::Video, "/workspace/media/clip.mp4", None),
                file(
                    MediaKind::File,
                    "/workspace/media/page.html",
                    Some("text/html"),
                ),
                file(
                    MediaKind::Image,
                    "/workspace/media/logo.svg",
                    Some("image/svg+xml"),
                ),
            ] {
                assert!(matches!(
                    gateway.send_media(request).await,
                    Ok(GatewaySendMediaResponse::Sent(_))
                ));
            }
            // a stream is spooled to the Gateway's storage, then kept as a file
            for request in [
                json!({"action": "start", "stream_id": "s1", "sequence": 0, "channel": "web",
                       "conversation_id": "conversation", "thread_id": null, "kind": "audio",
                       "filename": "voice.ogg", "mime_type": "audio/ogg", "caption": "listen",
                       "reply_to": "web-in-1"}),
                json!({"action": "chunk", "stream_id": "s1", "sequence": 1,
                       "content_base64": "aGVsbG8="}),
                json!({"action": "finish", "stream_id": "s1", "sequence": 2}),
            ] {
                let request = serde_json::from_value(request).expect("a stream command");
                assert!(matches!(
                    gateway.send_media(request).await,
                    Ok(GatewaySendMediaResponse::Accepted(_))
                ));
            }
            until("four media events", || {
                wire.events()
                    .iter()
                    .filter(|(event, _data)| event.starts_with("message."))
                    .count()
                    == 4
            })
            .await;
            let events = wire.events();
            let names: Vec<&str> = events.iter().map(|(event, _data)| event.as_str()).collect();
            assert_eq!(
                names,
                [
                    "message.video",
                    "message.file",
                    "message.image",
                    "message.audio"
                ]
            );
            let video = &events[0].1;
            assert_eq!(video["mime_type"], "video/mp4");
            assert_eq!(video["filename"], "clip.mp4");
            assert_eq!(video["size"], 5000);
            let audio = &events[3].1;
            assert_eq!(
                audio.as_object().unwrap().keys().collect::<Vec<_>>(),
                [
                    "caption",
                    "filename",
                    "message_id",
                    "mime_type",
                    "reply_to",
                    "size",
                    "url"
                ]
            );
            assert_eq!(audio["caption"], "listen");
            assert_eq!(audio["reply_to"], "web-in-1");
            assert_eq!(audio["size"], 5);
            // no event carries the bytes
            assert!(events
                .iter()
                .all(|(_event, data)| data.get("data").is_none()));

            let url = |index: usize| events[index].1["url"].as_str().unwrap().to_owned();
            let served = request(&server, "GET", &url(0), b"").await;
            assert_eq!(
                (served.status, served.content_type.as_str()),
                (200, "video/mp4")
            );
            assert_eq!(served.body, clip);
            for index in [1, 2] {
                let served = request(&server, "GET", &url(index), b"").await;
                assert_eq!(served.status, 200);
                assert_eq!(served.content_type, "application/octet-stream");
            }
            let served = request(&server, "GET", &url(3), b"").await;
            assert_eq!(
                (served.status, served.content_type.as_str()),
                (200, "audio/ogg")
            );
            assert_eq!(served.body, b"hello");

            let missing = request(&server, "GET", "/api/web/media/00000000-99", b"").await;
            assert_eq!(missing.status, 404);
            assert_eq!(missing.json(), json!({"error": "not_found"}));
            // a file that left the cache is as gone as an evicted id
            let spooled = filesystem
                .list_dir("/workspace/cache/gateway/web")
                .await
                .expect("the spooled stream");
            assert_eq!(spooled.len(), 1, "{spooled:?}");
            filesystem
                .remove_file(&format!("/workspace/cache/gateway/web/{}", spooled[0]))
                .await
                .expect("clear the cache");
            assert_eq!(request(&server, "GET", &url(3), b"").await.status, 404);
        })
        .await;
    })
}

rusty_fork_test! {
    #[test]
    fn an_upload_streams_into_the_gateways_storage() {
        run(stores_an_upload);
    }

    #[test]
    fn a_refused_upload_says_why() {
        run(refuses_bad_uploads);
    }

    #[test]
    fn a_message_frame_sends_its_uploads_once() {
        run(sends_uploads_with_a_message);
    }

    #[test]
    fn sent_media_is_one_event_and_a_url_served_by_its_type() {
        run(serves_sent_media);
    }
}

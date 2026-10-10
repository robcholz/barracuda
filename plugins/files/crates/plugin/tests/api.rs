//! The `/api/files` routes, driven through the WebServer as real HTTP requests.

#![allow(clippy::expect_used, clippy::panic)]
#![recursion_limit = "256"]

use std::{cell::RefCell, rc::Rc};

use barracuda_captive_portal_plugin::CaptivePortalPlugin;
use barracuda_files_plugin::FilesPlugin;
use barracuda_platform_test::{MemoryPartition, loopback_network, memory_partition};
use barracuda_plugin::{
    api::{ClientFactory, Hardware, PlatformInfo, PluginContext, TargetIdentity},
    manager::{Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult},
};
use barracuda_vfs::{MountOptions, Vfs};
use barracuda_vfs_memfs::MemFs;
use barracuda_webserver_plugin::{WebServer, WebServerPlugin};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use picoserve::io::Socket;
use picoserve::time::{Duration, TimeoutError, Timer};

struct Observer(Rc<RefCell<Option<Rc<WebServer>>>>);

impl PluginDeclaration for Observer {
    const ID: &'static str = "observer";
    const DEPENDS_ON: &'static [&'static str] = &["webserver", "files"];
}

impl Plugin for Observer {
    fn register<Storage: barracuda_plugin::manager::PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        self.0
            .replace(Some(context.require::<WebServer>(Self::DEPENDS_ON[0])?));
        Ok(())
    }
}

/// A WebServer with the portal and the file browser over an in-memory System
/// filesystem, where Plugin `agent` already keeps a note.
async fn server() -> (PluginManager<MemoryPartition>, Rc<WebServer>) {
    let network = loopback_network();
    let stack = network.stack();
    let identity = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        barracuda_plugin::api::BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    let mut context = PluginContext::new(identity, stack, ClientFactory::plaintext(stack));
    let resources = MemFs::new();
    for directory in ["/plugins/captive-portal", "/plugins/files", "/workspace"] {
        resources.create_dir_all(directory).expect("resources");
    }
    resources
        .write_file("/plugins/captive-portal/index.html", b"portal")
        .expect("portal fixture");
    resources
        .write_file("/plugins/files/entry.js", b"files")
        .expect("files fixture");
    let filesystem = Vfs::new();
    let durable = MemFs::new().into_backend();
    filesystem
        .mount("/data", durable.clone(), MountOptions::read_write())
        .await
        .expect("data volume");
    filesystem
        .create_dir_all("/data/media")
        .await
        .expect("media root");
    filesystem
        .mount_scoped("/media", durable, "/media", MountOptions::read_write())
        .await
        .expect("media volume");
    filesystem
        .mount(
            "/resources",
            resources.into_backend(),
            MountOptions::read_only(),
        )
        .await
        .expect("resources volume");
    for (point, options) in [
        ("/cache", MountOptions::read_write()),
        ("/removable", MountOptions::read_only()),
    ] {
        filesystem
            .mount(point, MemFs::new().into_backend(), options)
            .await
            .expect("volume");
    }
    filesystem
        .create_dir_all("/data/plugins/agent")
        .await
        .expect("agent data");
    filesystem
        .write("/data/plugins/agent/notes.txt", b"remember")
        .await
        .expect("agent note");
    let partition = memory_partition(64 * 1024).await.expect("partition");
    let mut manager = PluginManager::open(partition).await.expect("manager");
    manager.install_vfs(filesystem);
    manager
        .register(WebServerPlugin::new(&mut context))
        .expect("webserver");
    manager
        .register(CaptivePortalPlugin::new(&mut context))
        .expect("portal");
    manager
        .register(FilesPlugin::new(&mut context))
        .expect("files");
    let observed = Rc::new(RefCell::new(None));
    manager
        .register(Observer(Rc::clone(&observed)))
        .expect("observer");
    // The manager owns every route, so it lives as long as the server is used.
    (manager, observed.take().expect("server"))
}

struct Response {
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

impl Response {
    fn text(&self) -> &str {
        std::str::from_utf8(&self.body).expect("UTF-8 body")
    }
}

async fn request(server: &WebServer, method: &str, path: &str, body: &[u8]) -> Response {
    let mut input = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    input.extend_from_slice(body);
    send(server, input).await
}

async fn send(server: &WebServer, input: Vec<u8>) -> Response {
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
        .expect("serve");
    let bytes = output.0;
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("headers");
    let head = std::str::from_utf8(&bytes[..split]).expect("ASCII headers");
    let status = head[9..12].parse().expect("status");
    let content_type = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Type: "))
        .unwrap_or_default()
        .to_owned();
    Response {
        status,
        content_type,
        body: bytes[split + 4..].to_vec(),
    }
}

async fn post(server: &WebServer, path: &str, body: &str) -> Response {
    request(server, "POST", path, body.as_bytes()).await
}

#[tokio::test(flavor = "current_thread")]
async fn the_workspace_can_be_browsed_and_changed() {
    let (_manager, server) = server().await;

    let root = request(&server, "GET", "/api/files/list/workspace", b"").await;
    assert_eq!(root.status, 200);
    assert_eq!(
        root.text(),
        concat!(
            r#"{"entries":[{"name":"cache","type":"dir","size":0},"#,
            r#"{"name":"media","type":"dir","size":0},"#,
            r#"{"name":"removable","type":"dir","size":0},"#,
            r#"{"name":"resources","type":"dir","size":0}],"truncated":false}"#
        )
    );

    // A body four times the HTTP buffer streams to the file.
    let content: Vec<u8> = (0..20_000_u32)
        .map(|index| b'a' + (index % 26) as u8)
        .collect();
    let path = "/api/files/upload/workspace/media/run%20log.txt";
    let uploaded = request(&server, "PUT", path, &content).await;
    assert_eq!(uploaded.status, 201, "{}", uploaded.text());
    assert_eq!(
        uploaded.text(),
        r#"{"name":"run log.txt","type":"file","size":20000}"#
    );
    assert_eq!(request(&server, "PUT", path, b"again").await.status, 409);

    let raw = request(
        &server,
        "GET",
        "/api/files/raw/workspace/media/run%20log.txt",
        b"",
    )
    .await;
    assert_eq!(raw.status, 200);
    assert_eq!(raw.content_type, "text/plain; charset=utf-8");
    assert_eq!(raw.body, content);

    assert_eq!(
        post(
            &server,
            "/api/files/mkdir",
            r#"{"path":"/workspace/media/runs"}"#
        )
        .await
        .status,
        201
    );
    assert_eq!(
        post(
            &server,
            "/api/files/mkdir",
            r#"{"path":"/workspace/media/runs"}"#
        )
        .await
        .status,
        409
    );
    assert_eq!(
        post(
            &server,
            "/api/files/rename",
            r#"{"from":"/workspace/media/run log.txt","to":"/workspace/media/runs/a.txt"}"#
        )
        .await
        .status,
        400
    );
    assert_eq!(
        post(
            &server,
            "/api/files/rename",
            r#"{"from":"/workspace/media/run log.txt","to":"/workspace/media/a.html"}"#
        )
        .await
        .status,
        204
    );
    // Markup is served as bytes, never as a page under the portal's origin.
    assert_eq!(
        request(&server, "GET", "/api/files/raw/workspace/media/a.html", b"")
            .await
            .content_type,
        "application/octet-stream"
    );

    let listed = request(&server, "GET", "/api/files/list/workspace/media", b"").await;
    assert_eq!(
        listed.text(),
        concat!(
            r#"{"entries":[{"name":"runs","type":"dir","size":0},"#,
            r#"{"name":"a.html","type":"file","size":20000}],"truncated":false}"#
        )
    );

    // A body that ends early leaves nothing behind, not even its partial file.
    let cut = send(
        &server,
        b"PUT /api/files/upload/workspace/media/runs/cut.txt HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5000\r\nConnection: close\r\n\r\nonly this"
            .to_vec(),
    )
    .await;
    assert_eq!(cut.text(), r#"{"error":"incomplete_body"}"#);
    assert_eq!(
        request(&server, "GET", "/api/files/list/workspace/media/runs", b"")
            .await
            .text(),
        r#"{"entries":[],"truncated":false}"#
    );

    let into_runs = "/api/files/upload/workspace/media/runs/b.txt";
    assert_eq!(request(&server, "PUT", into_runs, b"b").await.status, 201);
    let not_empty = post(
        &server,
        "/api/files/delete",
        r#"{"path":"/workspace/media/runs"}"#,
    )
    .await;
    assert_eq!(not_empty.status, 409);
    assert_eq!(not_empty.text(), r#"{"error":"not_empty"}"#);
    for path in ["runs/b.txt", "runs", "a.html"] {
        let body = format!(r#"{{"path":"/workspace/media/{path}"}}"#);
        assert_eq!(post(&server, "/api/files/delete", &body).await.status, 204);
    }
    assert_eq!(
        request(&server, "GET", "/api/files/list/workspace/media", b"")
            .await
            .text(),
        r#"{"entries":[],"truncated":false}"#
    );

    // The read-only trees refuse in the file layer.
    assert_eq!(
        request(
            &server,
            "PUT",
            "/api/files/upload/workspace/resources/x",
            b"x"
        )
        .await
        .status,
        403
    );
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_files_are_readable_and_never_change() {
    let (_manager, server) = server().await;

    let data = request(&server, "GET", "/api/files/list/plugins/data", b"").await;
    assert!(
        data.text()
            .contains(r#"{"name":"agent","type":"dir","size":0}"#)
    );
    let note = request(
        &server,
        "GET",
        "/api/files/raw/plugins/data/agent/notes.txt",
        b"",
    )
    .await;
    assert_eq!(note.status, 200);
    assert_eq!(note.body, b"remember");

    let refused = [
        request(
            &server,
            "PUT",
            "/api/files/upload/plugins/data/agent/x.txt",
            b"x",
        )
        .await,
        post(
            &server,
            "/api/files/delete",
            r#"{"path":"/plugins/data/agent/notes.txt"}"#,
        )
        .await,
        post(
            &server,
            "/api/files/mkdir",
            r#"{"path":"/plugins/data/agent/new"}"#,
        )
        .await,
        post(
            &server,
            "/api/files/rename",
            r#"{"from":"/plugins/data/agent/notes.txt","to":"/plugins/data/agent/n.txt"}"#,
        )
        .await,
    ];
    for response in refused {
        assert_eq!(response.status, 403, "{}", response.text());
        assert_eq!(response.text(), r#"{"error":"read_only"}"#);
    }
    assert_eq!(
        request(
            &server,
            "GET",
            "/api/files/raw/plugins/data/agent/notes.txt",
            b""
        )
        .await
        .body,
        b"remember"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn only_workspace_and_plugin_paths_are_reached() {
    let (_manager, server) = server().await;
    for path in [
        "/api/files/list/data",
        "/api/files/list/resources",
        "/api/files/list/workspace/%2E%2E/data",
        "/api/files/list/workspace/..%5Cdata",
        "/api/files/raw/workspace//media",
    ] {
        let response = request(&server, "GET", path, b"").await;
        assert_eq!(response.status, 400, "{path}");
    }
    assert_eq!(
        post(&server, "/api/files/delete", r#"{"path":"/data/state"}"#)
            .await
            .status,
        400
    );
    assert_eq!(
        request(
            &server,
            "GET",
            "/api/files/list/workspace/media/absent",
            b""
        )
        .await
        .status,
        404
    );
    assert_eq!(
        request(&server, "GET", "/api/files/raw/workspace/media", b"")
            .await
            .status,
        409
    );
    assert_eq!(
        request(&server, "GET", "/api/files/mkdir", b"")
            .await
            .status,
        405
    );

    let manifest = request(&server, "GET", "/portal/entries.json", b"").await;
    assert!(manifest.text().contains(r#""id":"files","group":"device""#));
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

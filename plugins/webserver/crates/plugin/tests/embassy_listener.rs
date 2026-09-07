//! Embassy Net listener end-to-end tests.

#![allow(clippy::expect_used)]
#![recursion_limit = "256"]

use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use barracuda_platform_test::loopback_network;
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpProvider, HttpRequest, HttpResponse, WebServer,
    WebSocketConnection, WebSocketEndpoint, WebSocketFuture,
};
use embassy_net::{tcp::TcpSocket, Ipv4Address, Stack};
use embedded_io_async::Write as _;
use picoserve::time::EmbassyTimer;

const PORT: u16 = 8787;

struct PathEndpoint;

impl HttpEndpoint for PathEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(
            async move { HttpResponse::new(200, "text/plain", request.path().as_bytes().to_vec()) },
        )
    }
}

struct StreamEndpoint {
    declared: usize,
    available: usize,
    fail: bool,
    reads: Rc<RefCell<Vec<usize>>>,
}

struct Reader {
    remaining: usize,
    fail: bool,
    reads: Rc<RefCell<Vec<usize>>>,
}

impl embedded_io_async::ErrorType for Reader {
    type Error = embedded_io_async::ErrorKind;
}

impl embedded_io_async::Read for Reader {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.reads.borrow_mut().push(buffer.len());
        if self.fail {
            return Err(embedded_io_async::ErrorKind::Other);
        }
        let count = self.remaining.min(buffer.len()).min(333);
        buffer[..count].fill(b'x');
        self.remaining -= count;
        Ok(count)
    }
}

impl HttpProvider for StreamEndpoint {
    async fn serve(&self, _path: &str) -> HttpResponse {
        HttpResponse::stream(
            200,
            "application/octet-stream",
            self.declared,
            Reader {
                remaining: self.available,
                fail: self.fail,
                reads: Rc::clone(&self.reads),
            },
        )
    }
}

async fn roundtrip(server: Rc<WebServer>, path: &str) -> (Vec<u8>, Result<(), String>) {
    let network = loopback_network();
    let stack = network.stack();
    let exchange = async {
        let client = async {
            let mut socket = connect(stack).await;
            socket
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .expect("request");
            let mut bytes = Vec::new();
            let mut buffer = [0; 512];
            loop {
                match socket.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                }
                if let Some(boundary) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..boundary]).expect("headers");
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().expect("length"))
                        })
                        .expect("content length");
                    if bytes.len() >= boundary + 4 + length {
                        break;
                    }
                }
            }
            bytes
        };
        tokio::select! {
            result = serve_once(server, stack) => (result, Vec::new()),
            bytes = client => (Ok(()), bytes),
        }
    };
    let (result, bytes) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! {
            () = network.run() => panic!("network stopped"),
            result = exchange => result,
        }
    })
    .await
    .expect("roundtrip timeout");
    (bytes, result)
}

#[tokio::test(flavor = "current_thread")]
async fn prefix_dispatch_preserves_encoded_path_and_exact_precedence() {
    let server = Rc::new(WebServer::new());
    let _prefix = server
        .serve_http_prefix("/assets", PathEndpoint)
        .expect("prefix");
    let exact = server
        .serve_http(
            "/assets/exact",
            RecordingHttpEndpoint {
                received: Rc::new(RefCell::new(None)),
            },
        )
        .expect("exact");
    let (bytes, result) = roundtrip(Rc::clone(&server), "/assets/a%20b.js?version=1").await;
    result.expect("serve prefix");
    assert!(bytes.ends_with(b"/assets/a%20b.js"));
    let (bytes, result) = roundtrip(Rc::clone(&server), "/assets/exact").await;
    result.expect("serve exact");
    assert!(bytes.starts_with(b"HTTP/1.1 201"));
    drop(exact);
    let (bytes, result) = roundtrip(Rc::clone(&server), "/assets/exact").await;
    result.expect("fallback after unload");
    assert!(bytes.ends_with(b"/assets/exact"));
    let (bytes, result) = roundtrip(server, "/assets-other").await;
    result.expect("serve 404");
    assert!(bytes.starts_with(b"HTTP/1.1 404"));
}

#[tokio::test(flavor = "current_thread")]
async fn streams_large_and_empty_bodies_with_bounded_reads() {
    for length in [0, 40_000] {
        let server = Rc::new(WebServer::new());
        let reads = Rc::new(RefCell::new(Vec::new()));
        let _route = server
            .serve(
                "/file",
                StreamEndpoint {
                    declared: length,
                    available: length + 10,
                    fail: false,
                    reads: Rc::clone(&reads),
                },
            )
            .expect("stream route");
        let (bytes, result) = roundtrip(server, "/file").await;
        result.expect("stream response");
        let boundary = bytes
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .expect("headers")
            + 4;
        let headers = std::str::from_utf8(&bytes[..boundary])
            .expect("headers UTF-8")
            .to_lowercase();
        assert!(headers.contains(&format!("content-length: {length}\r\n")));
        assert_eq!(&bytes[boundary..], vec![b'x'; length]);
        assert!(reads.borrow().iter().all(|size| *size <= 1024));
        if length == 0 {
            assert!(reads.borrow().is_empty());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn failed_or_short_sources_terminate_the_connection() {
    for fail in [false, true] {
        let server = Rc::new(WebServer::new());
        let _route = server
            .serve(
                "/file",
                StreamEndpoint {
                    declared: 40_000,
                    available: 100,
                    fail,
                    reads: Rc::new(RefCell::new(Vec::new())),
                },
            )
            .expect("stream route");
        let (_, result) = roundtrip(server, "/file").await;
        let error = result.expect_err("must report source failure");
        assert!(error.contains(if fail {
            "source failed"
        } else {
            "before Content-Length"
        }));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn keep_alive_requests_observe_route_removal() {
    let network = loopback_network();
    let stack = network.stack();
    let server = Rc::new(WebServer::from(
        picoserve::Config::const_default().keep_connection_alive(),
    ));
    let root = server
        .serve_http_prefix("/", PathEndpoint)
        .expect("fallback");
    let route = server
        .serve_http(
            "/api",
            RecordingHttpEndpoint {
                received: Rc::new(RefCell::new(None)),
            },
        )
        .expect("exact");
    let exchange = async {
        let client = async {
            let mut socket = connect(stack).await;
            socket
                .write_all(b"GET /api HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .expect("first request");
            let bytes = read_response(&mut socket, br#"{"ok":true}"#).await;
            assert!(bytes.starts_with(b"HTTP/1.1 201"));
            drop(route);
            socket
                .write_all(b"GET /api HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .expect("second request");
            let bytes = read_response(&mut socket, b"/api").await;
            assert!(bytes.starts_with(b"HTTP/1.1 200"));
            drop(root);
            socket
                .write_all(b"GET /api HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .expect("third request");
            let bytes = read_response(&mut socket, b"Web endpoint not found").await;
            assert!(bytes.starts_with(b"HTTP/1.1 404"));
        };
        tokio::select! {
            result = serve_once(server, stack) => panic!("server stopped: {result:?}"),
            () = client => {}
        }
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! {
            () = network.run() => panic!("network stopped"),
            () = exchange => {}
        }
    })
    .await
    .expect("keep-alive exchange");
}

struct EmptyEndpoint;
struct HoldingEndpoint;

type ReceivedRequest = Rc<RefCell<Option<(HttpMethod, Vec<u8>)>>>;

struct RecordingHttpEndpoint {
    received: ReceivedRequest,
}

impl HttpEndpoint for RecordingHttpEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            *self.received.borrow_mut() = Some((request.method(), request.body().to_vec()));
            HttpResponse::new(201, "application/json", br#"{"ok":true}"#.to_vec())
        })
    }
}

impl WebSocketEndpoint for EmptyEndpoint {
    fn connected<'a>(&'a self, _connection: WebSocketConnection) -> WebSocketFuture<'a> {
        Box::pin(async {})
    }
}

impl WebSocketEndpoint for HoldingEndpoint {
    fn connected<'a>(&'a self, _connection: WebSocketConnection) -> WebSocketFuture<'a> {
        Box::pin(core::future::pending())
    }
}

async fn serve_once(server: Rc<WebServer>, stack: Stack<'static>) -> Result<(), String> {
    let mut tcp_rx = [0_u8; 4096];
    let mut tcp_tx = [0_u8; 4096];
    let mut http = [0_u8; 8192];
    let mut socket = TcpSocket::new(stack, &mut tcp_rx, &mut tcp_tx);
    socket
        .accept(PORT)
        .await
        .map_err(|error| format!("accept failed: {error:?}"))?;
    server
        .serve_connection(EmbassyTimer, &mut http, socket)
        .await
        .map_err(|error| format!("serve failed: {error}"))?;
    Ok(())
}

async fn connect(stack: Stack<'static>) -> TcpSocket<'static> {
    let rx = Box::leak(Box::new([0_u8; 4096]));
    let tx = Box::leak(Box::new([0_u8; 4096]));
    let mut socket = TcpSocket::new(stack, rx, tx);
    socket
        .connect((Ipv4Address::new(10, 0, 0, 1), PORT))
        .await
        .expect("connect to WebServer");
    socket
}

async fn read_response(socket: &mut TcpSocket<'_>, expected_body: &[u8]) -> Vec<u8> {
    let mut response = Vec::new();
    let mut chunk = [0_u8; 128];
    loop {
        let length = socket.read(&mut chunk).await.expect("read response");
        response.extend_from_slice(&chunk[..length]);
        if length == 0 || response.ends_with(expected_body) {
            return response;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn embassy_stack_accepts_a_real_websocket_upgrade() {
    let network = loopback_network();
    let stack = network.stack();
    let server = Rc::new(WebServer::new());
    let _fallback = server
        .serve_http_prefix("/", PathEndpoint)
        .expect("HTTP fallback");
    let _route = server
        .serve_websocket("/chat", EmptyEndpoint)
        .expect("register test route");

    let exchange = async {
        let server = serve_once(Rc::clone(&server), stack);
        let client = async {
            let mut socket = connect(stack).await;
            socket
                .write_all(
                    b"GET /chat HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
                )
                .await
                .expect("write WebSocket upgrade");
            let mut response = [0_u8; 512];
            let length = socket.read(&mut response).await.expect("read upgrade");
            assert!(response[..length].starts_with(b"HTTP/1.1 101 \r\n"));
        };
        tokio::select! {
            result = server => panic!("server stopped before client completed: {result:?}"),
            () = client => {}
        }
    };

    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            () = network.run() => panic!("network runner stopped"),
            () = exchange => {}
        }
    })
    .await
    .expect("WebSocket exchange completes");
}

#[tokio::test(flavor = "current_thread")]
async fn embassy_stack_serves_a_registered_http_endpoint() {
    let network = loopback_network();
    let stack = network.stack();
    let received = Rc::new(RefCell::new(None));
    let server = Rc::new(WebServer::new());
    let _route = server
        .serve_http(
            "/configure",
            RecordingHttpEndpoint {
                received: Rc::clone(&received),
            },
        )
        .expect("register HTTP route");

    let exchange = async {
        let server = serve_once(Rc::clone(&server), stack);
        let client = async {
            let mut socket = connect(stack).await;
            socket
                .write_all(
                    b"POST /configure HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                )
                .await
                .expect("write HTTP request");
            let expected_body = br#"{"ok":true}"#;
            let response = read_response(&mut socket, expected_body).await;
            assert!(response.starts_with(b"HTTP/1.1 201 \r\n"));
            assert!(response.ends_with(expected_body));
        };
        tokio::select! {
            result = server => panic!("server stopped before client completed: {result:?}"),
            () = client => {}
        }
    };

    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            () = network.run() => panic!("network runner stopped"),
            () = exchange => {}
        }
    })
    .await
    .expect("HTTP exchange completes");

    assert_eq!(
        received.borrow().as_ref(),
        Some(&(HttpMethod::Post, b"hello".to_vec()))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn long_lived_websocket_does_not_block_http_endpoint() {
    let network = loopback_network();
    let stack = network.stack();
    let received = Rc::new(RefCell::new(None));
    let server = Rc::new(WebServer::new());
    let _websocket_route = server
        .serve_websocket("/chat", HoldingEndpoint)
        .expect("register WebSocket route");
    let _http_route = server
        .serve_http(
            "/configure",
            RecordingHttpEndpoint {
                received: Rc::clone(&received),
            },
        )
        .expect("register HTTP route");

    let exchange = async {
        let listeners = embassy_futures::join::join(
            serve_once(Rc::clone(&server), stack),
            serve_once(Rc::clone(&server), stack),
        );
        let client = async {
            let mut websocket = connect(stack).await;
            websocket
                .write_all(
                    b"GET /chat HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
                )
                .await
                .expect("write WebSocket upgrade");
            let mut upgrade = [0_u8; 512];
            let length = websocket.read(&mut upgrade).await.expect("read upgrade");
            assert!(upgrade[..length].starts_with(b"HTTP/1.1 101 \r\n"));

            let mut http = connect(stack).await;
            http.write_all(
                b"POST /configure HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            )
            .await
            .expect("write HTTP request");
            let expected_body = br#"{"ok":true}"#;
            let response = read_response(&mut http, expected_body).await;
            assert!(response.starts_with(b"HTTP/1.1 201 \r\n"));
            assert!(response.ends_with(expected_body));
            drop(websocket);
        };
        tokio::select! {
            result = listeners => panic!("listeners stopped: {result:?}"),
            () = client => {}
        }
    };

    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            () = network.run() => panic!("network runner stopped"),
            () = exchange => {}
        }
    })
    .await
    .expect("HTTP request completes while WebSocket remains open");

    assert_eq!(
        received.borrow().as_ref(),
        Some(&(HttpMethod::Post, b"hello".to_vec()))
    );
}

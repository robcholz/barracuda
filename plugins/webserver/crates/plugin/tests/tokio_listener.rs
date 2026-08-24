//! Tokio listener end-to-end tests.

#![allow(clippy::expect_used)]

use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use barracuda_platform_host::TokioStack;
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer, WebServerListener,
    WebSocketConnection, WebSocketEndpoint, WebSocketFuture, WEB_SERVER_CONNECTION_SLOTS,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

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

#[tokio::test(flavor = "current_thread")]
async fn tokio_stack_accepts_a_real_websocket_upgrade() {
    let available =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("reserve test port");
    let port = available.local_addr().expect("read test address").port();
    drop(available);

    let server = Rc::new(WebServer::new());
    let _route = server
        .serve("/chat", EmptyEndpoint)
        .expect("register test route");
    let mut network = TokioStack::default();

    let listener = network.listen(Rc::clone(&server), port, WEB_SERVER_CONNECTION_SLOTS);
    let client = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .expect("connect to WebServer");
        socket
            .write_all(
                b"GET /chat HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .await
            .expect("write WebSocket upgrade");
        let mut response = [0_u8; 512];
        let length = socket
            .read(&mut response)
            .await
            .expect("read upgrade response");
        assert!(
            response[..length].starts_with(b"HTTP/1.1 101 \r\n"),
            "{}",
            String::from_utf8_lossy(&response[..length]),
        );
    };

    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            result = listener => panic!("listener stopped unexpectedly: {result:?}"),
            () = client => {}
        }
    })
    .await
    .expect("WebSocket upgrade completes");
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_stack_serves_a_registered_http_endpoint() {
    let available =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("reserve test port");
    let port = available.local_addr().expect("read test address").port();
    drop(available);

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
    let mut network = TokioStack::default();

    let listener = network.listen(Rc::clone(&server), port, WEB_SERVER_CONNECTION_SLOTS);
    let client = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .expect("connect to WebServer");
        socket
            .write_all(
                b"POST /configure HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            )
            .await
            .expect("write HTTP request");
        let expected_body = br#"{"ok":true}"#;
        let mut response = Vec::new();
        let mut chunk = [0_u8; 128];
        loop {
            let length = socket.read(&mut chunk).await.expect("read HTTP response");
            response.extend_from_slice(&chunk[..length]);
            if length == 0 || response.ends_with(expected_body) {
                break;
            }
        }
        assert!(response.starts_with(b"HTTP/1.1 201 \r\n"));
        assert!(response
            .windows(b"Content-Type: application/json".len())
            .any(|window| window == b"Content-Type: application/json"));
        assert!(response.ends_with(expected_body));
    };

    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            result = listener => panic!("listener stopped unexpectedly: {result:?}"),
            () = client => {}
        }
    })
    .await
    .expect("HTTP request completes");

    assert_eq!(
        received.borrow().as_ref(),
        Some(&(HttpMethod::Post, b"hello".to_vec()))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn long_lived_websocket_does_not_block_http_endpoint() {
    let available =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("reserve test port");
    let port = available.local_addr().expect("read test address").port();
    drop(available);

    let received = Rc::new(RefCell::new(None));
    let server = Rc::new(WebServer::new());
    let _websocket_route = server
        .serve("/chat", HoldingEndpoint)
        .expect("register WebSocket route");
    let _http_route = server
        .serve_http(
            "/configure",
            RecordingHttpEndpoint {
                received: Rc::clone(&received),
            },
        )
        .expect("register HTTP route");
    let mut network = TokioStack::default();

    let listener = network.listen(Rc::clone(&server), port, WEB_SERVER_CONNECTION_SLOTS);
    let client = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut websocket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .expect("connect WebSocket");
        websocket
            .write_all(
                b"GET /chat HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .await
            .expect("write WebSocket upgrade");
        let mut upgrade = [0_u8; 512];
        let length = websocket
            .read(&mut upgrade)
            .await
            .expect("read WebSocket upgrade");
        assert!(upgrade[..length].starts_with(b"HTTP/1.1 101 \r\n"));

        let mut http = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .expect("connect HTTP while WebSocket remains open");
        http.write_all(
            b"POST /configure HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
        )
        .await
        .expect("write HTTP request");

        let expected_body = br#"{"ok":true}"#;
        let mut response = Vec::new();
        let mut chunk = [0_u8; 128];
        loop {
            let length = http.read(&mut chunk).await.expect("read HTTP response");
            response.extend_from_slice(&chunk[..length]);
            if length == 0 || response.ends_with(expected_body) {
                break;
            }
        }
        assert!(response.starts_with(b"HTTP/1.1 201 \r\n"));
        assert!(response.ends_with(expected_body));

        drop(websocket);
    };

    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            result = listener => panic!("listener stopped unexpectedly: {result:?}"),
            () = client => {}
        }
    })
    .await
    .expect("HTTP request completes while WebSocket remains open");

    assert_eq!(
        received.borrow().as_ref(),
        Some(&(HttpMethod::Post, b"hello".to_vec()))
    );
}

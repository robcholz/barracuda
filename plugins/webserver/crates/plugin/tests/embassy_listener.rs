//! Embassy Net listener end-to-end tests.

#![allow(clippy::expect_used)]
#![recursion_limit = "256"]

use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use barracuda_platform_test::loopback_network;
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
    WebSocketConnection, WebSocketEndpoint, WebSocketFuture,
};
use embassy_net::{tcp::TcpSocket, Ipv4Address, Stack};
use embedded_io_async::Write as _;
use picoserve::time::EmbassyTimer;

const PORT: u16 = 8787;

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
    let _route = server
        .serve("/chat", EmptyEndpoint)
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

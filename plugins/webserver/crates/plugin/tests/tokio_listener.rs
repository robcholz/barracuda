//! Tokio listener end-to-end tests.

#![cfg(feature = "tokio")]
#![allow(clippy::expect_used)]

use std::boxed::Box;
use std::time::Duration;

use barracuda_net::TokioStack;
use barracuda_webserver_plugin::{
    WebServer, WebServerListener, WebSocketConnection, WebSocketEndpoint, WebSocketFuture,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct EmptyEndpoint;

impl WebSocketEndpoint for EmptyEndpoint {
    fn connected<'a>(&'a self, _connection: WebSocketConnection) -> WebSocketFuture<'a> {
        Box::pin(async {})
    }
}

#[tokio::test(flavor = "current_thread")]
async fn tokio_stack_accepts_a_real_websocket_upgrade() {
    let available =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("reserve test port");
    let port = available.local_addr().expect("read test address").port();
    drop(available);

    let server = WebServer::new();
    let _route = server
        .serve("/chat", EmptyEndpoint)
        .expect("register test route");
    let mut network = TokioStack;

    let listener = network.listen(&server, port);
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

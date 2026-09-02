//! Real socket coverage for the Web provider's bidirectional bridge.

#![allow(clippy::expect_used, clippy::indexing_slicing)]
#![recursion_limit = "256"]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use barracuda_platform_test::loopback_network;
use barracuda_webserver_plugin::WebServer;
use embassy_net::{tcp::TcpSocket, Ipv4Address, Stack};
use embedded_io_async::{Read as _, Write as _};
use gateway::{MessageChannel, MessageTarget, SendMessageRequest};
use picoserve::time::EmbassyTimer;
use web::{InboundFuture, InboundMessage, InboundMessageSink, Web, WebBridge};

const PORT: u16 = 8787;

#[derive(Default)]
struct RecordingSink {
    messages: RefCell<Vec<InboundMessage>>,
}

impl InboundMessageSink for RecordingSink {
    fn receive_message(&self, request: InboundMessage) -> InboundFuture<'_, ()> {
        Box::pin(async move {
            self.messages.borrow_mut().push(request);
            Ok(())
        })
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
        .map(|_disconnection| ())
        .map_err(|error| format!("serve failed: {error}"))
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

fn masked_text(text: &str) -> Vec<u8> {
    let payload = text.as_bytes();
    assert!(payload.len() < 126);
    let mask = [1_u8, 2, 3, 4];
    let mut frame = vec![0x81, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()]),
    );
    frame
}

async fn read_text_frame(socket: &mut TcpSocket<'_>) -> String {
    let mut header = [0_u8; 2];
    socket
        .read_exact(&mut header)
        .await
        .expect("read frame header");
    assert_eq!(header[0] & 0x0f, 1);
    let mut length = usize::from(header[1] & 0x7f);
    if length == 126 {
        let mut extended = [0_u8; 2];
        socket
            .read_exact(&mut extended)
            .await
            .expect("read extended frame length");
        length = usize::from(u16::from_be_bytes(extended));
    }
    assert_ne!(length, 127);
    let mut payload = vec![0_u8; length];
    socket
        .read_exact(&mut payload)
        .await
        .expect("read frame payload");
    String::from_utf8(payload).expect("server text is UTF-8")
}

#[tokio::test(flavor = "current_thread")]
async fn real_websocket_bridges_inbound_json_and_outbound_gateway_events() {
    let network = loopback_network();
    let stack = network.stack();
    let web = Rc::new(Web::<16, 2>::new());
    let sink = Rc::new(RecordingSink::default());
    let server = Rc::new(WebServer::new());
    let _route = server
        .serve(
            "/chat",
            WebBridge::new(web.clone(), sink.clone(), "conversation"),
        )
        .expect("register Web bridge");

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
            let mut upgrade = [0_u8; 512];
            let length = socket.read(&mut upgrade).await.expect("read upgrade");
            assert!(upgrade[..length].starts_with(b"HTTP/1.1 101 \r\n"));

            socket
                .write_all(&masked_text(r#"{"text":"hello","reply_to":"previous"}"#))
                .await
                .expect("send client message");
            while sink.messages.borrow().is_empty() {
                futures_lite::future::yield_now().await;
            }
            let inbound = sink.messages.borrow()[0].clone();
            assert_eq!(inbound.conversation_id, "conversation");
            assert_eq!(inbound.message_id, "web-in-1");
            assert_eq!(inbound.text, "hello");
            assert_eq!(inbound.reply_to.as_deref(), Some("previous"));

            web.send_message(SendMessageRequest::text(
                MessageTarget::new("web", "conversation"),
                "reply",
            ))
            .await
            .expect("publish Gateway reply");
            let start = read_text_frame(&mut socket).await;
            let delta = read_text_frame(&mut socket).await;
            let end = read_text_frame(&mut socket).await;
            assert!(start.contains("message.start"));
            assert!(delta.contains("message.delta"));
            assert!(delta.contains("reply"));
            assert!(end.contains("message.end"));
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
    .expect("Web bridge exchange completes");
}

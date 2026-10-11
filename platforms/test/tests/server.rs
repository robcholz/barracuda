//! The scripted server answers real connections on the loopback stack.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use barracuda_platform_test::{loopback_network, ScriptStep, ScriptedServer};
use embassy_net::{tcp::TcpSocket, Ipv4Address, Stack};
use embedded_io_async::Write as _;

const PORT: u16 = 9000;

async fn exchange(stack: Stack<'static>, request: &str) -> Result<String, ()> {
    let mut rx = vec![0; 4096];
    let mut tx = vec![0; 4096];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket
        .connect((Ipv4Address::new(10, 0, 0, 1), PORT))
        .await
        .map_err(|_| ())?;
    socket.write_all(request.as_bytes()).await.map_err(|_| ())?;
    let mut bytes = Vec::new();
    let mut buffer = [0; 512];
    loop {
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
        }
        if bytes.ends_with(b"}") {
            break;
        }
    }
    if bytes.is_empty() {
        return Err(());
    }
    Ok(String::from_utf8(bytes).expect("UTF-8 response"))
}

#[tokio::test(flavor = "current_thread")]
async fn answers_in_order_records_requests_and_refuses_on_cue() {
    let network = loopback_network();
    let stack = network.stack();
    let server = ScriptedServer::new([
        ScriptStep::json(200, r#"{"first":1}"#),
        ScriptStep::ConnectError(embedded_io::ErrorKind::ConnectionReset),
        ScriptStep::json(201, r#"{"second":2}"#),
    ]);
    let client = async {
        let first = exchange(
            stack,
            "POST /one HTTP/1.1\r\nHost: test\r\nContent-Length: 5\r\n\r\nhello",
        )
        .await
        .expect("first response");
        assert!(first.starts_with("HTTP/1.1 200"));
        assert!(first.ends_with(r#"{"first":1}"#));
        assert!(
            exchange(stack, "GET /refused HTTP/1.1\r\nHost: test\r\n\r\n")
                .await
                .is_err()
        );
        let second = exchange(
            stack,
            "POST /two HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n",
        )
        .await
        .expect("second response");
        assert!(second.starts_with("HTTP/1.1 201"));
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            () = network.run() => panic!("network stopped"),
            () = server.run::<2>(stack, PORT) => panic!("server stopped"),
            () = client => {}
        }
    })
    .await
    .expect("exchange within the timeout");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("POST /one ") && requests[0].ends_with("hello"));
    assert!(requests[1].starts_with("POST /two ") && requests[1].ends_with("0\r\n\r\n"));
    assert_eq!(server.remaining(), 0);
    assert_eq!(server.connect_count(), 3);
}

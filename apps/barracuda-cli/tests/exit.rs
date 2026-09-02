use std::io::Write as _;
use std::net::TcpListener;
use std::process::{Command, Stdio};

use futures_util::{SinkExt as _, StreamExt as _};

#[test]
fn invalid_command_exits_with_a_failure_and_actionable_diagnostic() {
    let output = Command::new(env!("CARGO_BIN_EXE_barracuda"))
        .arg("serve")
        .output()
        .expect("run barracuda CLI");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        "error: unknown subcommand `serve`; use `connect <url>` or `chat`\n"
    );
}

#[test]
fn connect_failure_exits_cleanly_with_the_target_url() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve unused port");
    let address = listener.local_addr().expect("read reserved address");
    drop(listener);
    let url = format!("ws://{address}");

    let output = Command::new(env!("CARGO_BIN_EXE_barracuda"))
        .args(["connect", &url])
        .output()
        .expect("run Barracuda CLI");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
    assert!(stderr.contains(&format!("failed to connect to {url}")));
}

#[test]
fn connect_runs_the_real_terminal_websocket_lifecycle_until_quit() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test WebSocket server");
    let address = listener.local_addr().expect("read listener address");
    listener
        .set_nonblocking(true)
        .expect("Tokio requires nonblocking listener");
    let server = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build test runtime")
            .block_on(async move {
                let listener =
                    tokio::net::TcpListener::from_std(listener).expect("adopt test listener");
                let (connection, _) = listener.accept().await.expect("accept CLI connection");
                let mut websocket = tokio_tungstenite::accept_async(connection).await?;
                while websocket.next().await.is_some() {}
                Ok::<(), tokio_tungstenite::tungstenite::Error>(())
            })
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_barracuda"))
        .args(["connect", &format!("ws://{address}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start Barracuda CLI");
    child
        .stdin
        .take()
        .expect("CLI stdin is piped")
        .write_all(b"/quit\n")
        .expect("send quit command");
    let output = child.wait_with_output().expect("CLI exits");
    let server_result = server.join().expect("WebSocket server thread exits");

    let stderr = String::from_utf8(output.stderr).expect("terminal output is UTF-8");
    assert!(output.status.success(), "CLI failed: {stderr}");
    assert!(output.stdout.is_empty());
    assert!(
        server_result.is_ok(),
        "server failed: {server_result:?}; CLI: {stderr}"
    );
    assert!(stderr.contains(&format!("Connected to ws://{address}.")));
    assert!(stderr.contains("Goodbye."));
}

#[test]
fn terminal_client_sends_json_and_renders_a_real_gateway_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test WebSocket server");
    let address = listener.local_addr().expect("read listener address");
    listener
        .set_nonblocking(true)
        .expect("Tokio requires nonblocking listener");
    let (sent, received) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build test runtime")
            .block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("adopt test listener");
                let (connection, _) = listener.accept().await.expect("accept CLI connection");
                let mut websocket = tokio_tungstenite::accept_async(connection)
                    .await
                    .expect("complete WebSocket handshake");
                let request = websocket
                    .next()
                    .await
                    .expect("client sends a frame")
                    .expect("client frame is valid");
                let request = request.into_text().expect("client sends text");
                let request: serde_json::Value =
                    serde_json::from_str(&request).expect("client sends JSON");
                assert_eq!(request["text"], "hello server");
                assert!(request["reply_to"].is_null());

                for frame in [
                    "event: message.start\ndata: {\"kind\":\"reply\"}\n\n",
                    "event: message.delta\ndata: {\"delta\":\"hello back\"}\n\n",
                    "event: message.extra\ndata: {\"field\":\"notice\",\"content\":\"server note\",\"boundary\":\"complete\"}\n\n",
                    "event: message.end\ndata: {\"error\":\"stream warning\"}\n\n",
                ] {
                    websocket
                        .send(tokio_tungstenite::tungstenite::Message::text(frame))
                        .await
                        .expect("send gateway event");
                }
                sent.send(()).expect("notify test after response");
                while websocket.next().await.is_some() {}
            });
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_barracuda"))
        .args(["connect", &format!("ws://{address}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start Barracuda CLI");
    let mut stdin = child.stdin.take().expect("CLI stdin is piped");
    stdin
        .write_all(b"hello server\n")
        .expect("send chat message");
    received
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("server returns gateway stream");
    std::thread::sleep(std::time::Duration::from_millis(100));
    stdin.write_all(b"/quit\n").expect("send quit command");
    drop(stdin);

    let output = child.wait_with_output().expect("CLI exits");
    server.join().expect("WebSocket server exits");
    let stderr = String::from_utf8(output.stderr).expect("terminal output is UTF-8");
    assert!(output.status.success(), "CLI failed: {stderr}");
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("hello back"));
    assert!(stderr.contains("server note"));
    assert!(stderr.contains("stream warning"));
    assert!(stderr.contains("Goodbye."));
}

use std::io;
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::thread;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;

use super::hub;

/// WebSocket endpoint that streams context snapshots as JSON text frames.
const WEBSOCKET_PATH: &str = "/ws/context";
/// Lightweight endpoint used to check whether the observation server is alive.
const HEALTH_PATH: &str = "/healthz";

const READY_PAYLOAD: &str = r#"{"schema_version":1,"event":"ready"}"#;
const DEFAULT_PORT: u16 = 9464;

static SERVER_STARTED: OnceLock<()> = OnceLock::new();

pub(super) fn ensure_started() {
    let _started = SERVER_STARTED.get_or_init(|| {
        let spawn_result = thread::Builder::new()
            .name("claw-context-observer".to_owned())
            .spawn(run_server);
        match spawn_result {
            Ok(handle) => drop(handle),
            Err(error) => eprintln!("failed to start context observation thread: {error}"),
        }
    });
}

fn run_server() {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("failed to create context observation runtime: {error}");
            return;
        }
    };

    runtime.block_on(async {
        let address = SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT));
        let server = match ObservationServer::bind(address).await {
            Ok(server) => server,
            Err(error) => {
                eprintln!("failed to bind context observation server at {address}: {error}");
                return;
            }
        };
        if let Err(error) = server.run().await {
            eprintln!("context observation server stopped: {error}");
        }
    });
}

/// A bound intrusive-observability server.
struct ObservationServer {
    listener: tokio::net::TcpListener,
}

impl ObservationServer {
    /// Bind the server without starting its accept loop.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when `address` cannot be bound.
    async fn bind(address: SocketAddr) -> io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(address).await?;
        Ok(Self { listener })
    }

    /// Run the bound server until the returned future is cancelled.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the HTTP server fails.
    async fn run(self) -> io::Result<()> {
        let app = Router::new()
            .route(WEBSOCKET_PATH, get(websocket))
            .route(HEALTH_PATH, get(health));
        axum::serve(self.listener, app).await
    }
}

async fn health() -> &'static str {
    "ok"
}

async fn websocket(upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(stream_snapshots)
}

async fn stream_snapshots(socket: WebSocket) {
    let (mut outgoing, mut incoming) = socket.split();
    let mut snapshots = hub::subscribe();

    if outgoing
        .send(Message::Text(READY_PAYLOAD.into()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            snapshot = snapshots.recv() => {
                let payload = match snapshot {
                    Ok(payload) => payload,
                    Err(RecvError::Lagged(skipped)) => gap_payload(skipped),
                    Err(RecvError::Closed) => break,
                };
                if outgoing.send(Message::Text(payload.into())).await.is_err() {
                    break;
                }
            }
            message = incoming.next() => {
                match message {
                    Some(Ok(Message::Ping(payload))) => {
                        if outgoing.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(
                        Message::Text(_) | Message::Binary(_) | Message::Pong(_)
                    )) => {}
                }
            }
        }
    }
}

fn gap_payload(skipped: u64) -> String {
    serde_json::json!({
        "schema_version": 1,
        "event": "gap",
        "skipped": skipped,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::net::SocketAddr;

    use futures_util::StreamExt;
    use serde_json::{json, Value};
    use tokio_tungstenite::connect_async;

    use super::{ObservationServer, READY_PAYLOAD, WEBSOCKET_PATH};
    use crate::{Block, BlockKind, Context};

    #[tokio::test]
    async fn private_server_streams_request_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let server = ObservationServer::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let address = server.listener.local_addr()?;
        let server_task = tokio::spawn(server.run());

        let mut context = Context::new();
        let uri = format!("ws://{address}{WEBSOCKET_PATH}");
        let (mut websocket, _response) = connect_async(&uri).await?;

        let ready = websocket
            .next()
            .await
            .ok_or_else(|| io::Error::other("WebSocket closed before ready"))??;
        assert_eq!(ready.to_text()?, READY_PAYLOAD);

        const UNIQUE_SYSTEM: &str = "websocket-request-snapshot";
        context.with(Block::new(BlockKind::AgentInstruction, UNIQUE_SYSTEM));
        let history = json!([{ "role": "user", "content": "hello" }]);
        let _request = context.request(&history);

        let snapshot = loop {
            let message = websocket
                .next()
                .await
                .ok_or_else(|| io::Error::other("WebSocket closed before snapshot"))??;
            let snapshot: Value = serde_json::from_str(message.to_text()?)?;
            if snapshot.pointer("/system/rendered").and_then(Value::as_str) == Some(UNIQUE_SYSTEM) {
                break snapshot;
            }
        };
        assert_eq!(snapshot.pointer("/event"), Some(&json!("context_snapshot")));
        assert_eq!(
            snapshot.pointer("/system/blocks/0/kind/name"),
            Some(&json!("agent_instruction"))
        );

        server_task.abort();
        Ok(())
    }
}

//! Host-side assembly for the CLI's default local Barracuda system.

use anyhow::{anyhow, Result};
use barracuda_event_router::RpcLaneStorage;
use barracuda_system::System;
use embassy_executor::Spawner;

const COMPONENTS: usize = 32;
const RPC_MESSAGE_BYTES: usize = 512;
const RPC_QUEUE_DEPTH: usize = 8;

pub(crate) type LocalSystem =
    System<COMPONENTS, RPC_MESSAGE_BYTES, RPC_QUEUE_DEPTH, barracuda_target::DatabaseRegion>;

/// Builds the complete local System owned by the default CLI mode.
pub(crate) async fn build(spawner: Spawner) -> Result<LocalSystem> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
    let resources = barracuda_target::resources(spawner)
        .await
        .map_err(|error| anyhow!("failed to construct selected target resources: {error}"))?;
    System::new(lanes, resources, "/workflows")
        .await
        .map_err(|error| anyhow!("failed to construct local Barracuda System: {error}"))
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::mpsc::{sync_channel, SyncSender};
    use std::time::Duration;

    use anyhow::{anyhow, Result};
    use embassy_executor::{Executor, Spawner};
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::Message;

    use super::build;
    use crate::protocol::parse_sse;
    use crate::DEFAULT_URL;

    #[embassy_executor::task]
    async fn websocket_smoke_task(
        spawner: Spawner,
        completed: SyncSender<std::result::Result<(), String>>,
    ) {
        let result = websocket_smoke(spawner)
            .await
            .map_err(|error| format!("{error:#}"));
        let _ignored = completed.send(result);
    }

    async fn websocket_smoke(spawner: Spawner) -> Result<()> {
        let system = build(spawner).await?;
        tokio::pin!(system);

        let connection = async {
            loop {
                match tokio_tungstenite::connect_async(DEFAULT_URL).await {
                    Ok(connection) => break connection,
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        };

        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                result = &mut system => Err(anyhow!("System stopped before accepting a WebSocket: {result:?}")),
                connection = connection => {
                    let (_socket, response) = connection;
                    if response.status() == 101 {
                        Ok(())
                    } else {
                        Err(anyhow!("unexpected WebSocket response: {}", response.status()))
                    }
                }
            }
        })
        .await
        .map_err(|_elapsed| anyhow!("local WebSocket listener did not become ready"))?
    }

    #[embassy_executor::task]
    async fn live_https_smoke_task(
        spawner: Spawner,
        completed: SyncSender<std::result::Result<(), String>>,
    ) {
        let result = live_https_smoke(spawner)
            .await
            .map_err(|error| format!("{error:#}"));
        let _ignored = completed.send(result);
    }

    async fn live_https_smoke(spawner: Spawner) -> Result<()> {
        let system = build(spawner).await?;
        tokio::pin!(system);

        let conversation = async {
            loop {
                if tokio::net::TcpStream::connect("127.0.0.1:8787")
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            let configure =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("configure-model-api.sh");
            let status =
                tokio::task::spawn_blocking(move || Command::new(configure).status()).await??;
            if !status.success() {
                return Err(anyhow!("model API configuration script failed: {status}"));
            }

            let (mut socket, _response) = tokio_tungstenite::connect_async(DEFAULT_URL).await?;
            let request = serde_json::to_string(&web::WebClientFrame {
                text: "Reply with exactly: tls-ok".into(),
                reply_to: None,
            })?;
            socket.send(Message::text(request)).await?;

            let mut reply = String::new();
            while let Some(message) = socket.next().await {
                let Message::Text(text) = message? else {
                    continue;
                };
                let Some(frame) = parse_sse(text.as_str()) else {
                    continue;
                };
                let data: serde_json::Value = serde_json::from_str(&frame.data)?;
                match frame.event.as_str() {
                    "message.delta" => {
                        if let Some(delta) = data.get("delta").and_then(serde_json::Value::as_str) {
                            reply.push_str(delta);
                        }
                    }
                    "message.end" if reply.contains("tls-ok") => return Ok(()),
                    "message.end" => {
                        return Err(anyhow!("unexpected model reply: {reply}"));
                    }
                    _ => {}
                }
            }
            Err(anyhow!("WebSocket closed before the model reply completed"))
        };

        tokio::time::timeout(Duration::from_secs(90), async {
            tokio::select! {
                result = &mut system => Err(anyhow!("System stopped during live HTTPS smoke test: {result:?}")),
                result = conversation => result,
            }
        })
        .await
        .map_err(|_elapsed| anyhow!("live HTTPS model request timed out"))?
    }

    #[test]
    fn local_system_accepts_the_default_websocket() {
        let (completed, result) = sync_channel(1);
        std::thread::spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner
                    .spawn(websocket_smoke_task(spawner, completed))
                    .expect("spawn WebSocket smoke task");
            });
        });

        result
            .recv_timeout(Duration::from_secs(5))
            .expect("WebSocket smoke task timed out")
            .expect("local System WebSocket smoke failed");
    }

    #[test]
    #[ignore = "requires apps/barracuda-cli/.env.local and live model API access"]
    fn live_model_api_uses_https() {
        let (completed, result) = sync_channel(1);
        std::thread::spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner
                    .spawn(live_https_smoke_task(spawner, completed))
                    .expect("spawn live HTTPS smoke task");
            });
        });

        result
            .recv_timeout(Duration::from_secs(100))
            .expect("live HTTPS smoke task timed out")
            .expect("live HTTPS model request failed");
    }
}

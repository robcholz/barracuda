//! Host-side assembly for the CLI's default local Barracuda system.

use anyhow::{anyhow, Context as _, Result};
use barracuda_agent_plugin::{ModelApiFactory, RuntimeStorageConfig};
use barracuda_event_router::{MemFs, RpcLaneStorage};
use barracuda_model_api::ModelApi;
use barracuda_net::TokioStack;
use barracuda_plugin_manager::{EkvConfig, EkvStore, NoopRawMutex};
use barracuda_system::System;
use ekv::flash::MemFlash;

const COMPONENTS: usize = 32;
const RPC_MESSAGE_BYTES: usize = 512;
const RPC_QUEUE_DEPTH: usize = 8;
const HTTP_READ_BYTES: usize = 16 * 1024;
const HTTP_WRITE_BYTES: usize = 8 * 1024;

static NETWORK: TokioStack = TokioStack;

pub(crate) type LocalSystem = System<COMPONENTS, RPC_MESSAGE_BYTES, RPC_QUEUE_DEPTH>;

/// Builds the complete local System owned by the default CLI mode.
pub(crate) async fn build() -> Result<LocalSystem> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
    let workflow_filesystem = Box::leak(Box::new(MemFs::new()));
    let plugin_store =
        EkvStore::<MemFlash, NoopRawMutex>::new(MemFlash::new(), EkvConfig::default());
    plugin_store
        .format()
        .await
        .context("failed to initialize local Plugin storage")?;

    let model_api_factory =
        ModelApiFactory::new(|| ModelApi::new(&NETWORK, HTTP_READ_BYTES, HTTP_WRITE_BYTES));

    System::new(
        lanes,
        workflow_filesystem,
        "/workflows",
        plugin_store,
        TokioStack,
        TokioStack,
        MemFs::new(),
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        model_api_factory,
    )
    .await
    .map_err(|error| anyhow!("failed to construct local Barracuda System: {error}"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::build;
    use crate::DEFAULT_URL;

    #[tokio::test(flavor = "current_thread")]
    async fn local_system_accepts_the_default_websocket() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let system = build().await.expect("build local System");
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
                        result = &mut system => panic!("System stopped before accepting a WebSocket: {result:?}"),
                        connection = connection => {
                            let (_socket, response) = connection;
                            assert_eq!(response.status(), 101);
                        }
                    }
                })
                .await
                .expect("local WebSocket listener did not become ready");
            })
            .await;
    }
}

//! Native-platform assembly for the CLI's default local Barracuda system.

use anyhow::{anyhow, Result};
use barracuda_event_router::RpcLaneStorage;
use barracuda_system::System;
use embassy_executor::Spawner;

use crate::{client, DEFAULT_URL};

const COMPONENTS: usize = 32;
const RPC_MESSAGE_BYTES: usize = 512;
const RPC_QUEUE_DEPTH: usize = 8;

fn lanes() -> &'static RpcLaneStorage<COMPONENTS, RPC_MESSAGE_BYTES, RPC_QUEUE_DEPTH> {
    Box::leak(Box::new(RpcLaneStorage::<
        COMPONENTS,
        RPC_MESSAGE_BYTES,
        RPC_QUEUE_DEPTH,
    >::new()))
}

async fn target_resources(spawner: Spawner) -> Result<barracuda_target::Resources> {
    let resources = barracuda_target::resources(spawner)
        .await
        .map_err(|error| anyhow!("failed to construct selected target resources: {error}"))?;
    Ok(resources)
}

/// Runs the complete local System and shuts it down when its terminal client exits.
pub(crate) async fn run(spawner: Spawner) -> Result<()> {
    let mut system = System::new(lanes(), target_resources(spawner).await?, spawner)
        .await
        .map_err(|error| anyhow!("failed to construct local Barracuda System: {error}"))?;

    let client_result = tokio::select! {
        result = &mut system => return match result {
            Ok(()) => Err(anyhow!("local Barracuda System stopped")),
            Err(error) => Err(anyhow!("local Barracuda System stopped: {error}")),
        },
        result = client::run_when_available(DEFAULT_URL) => result,
    };

    system
        .shutdown()
        .await
        .map_err(|error| anyhow!("failed to shut down local Barracuda System: {error}"))?;
    client_result
}

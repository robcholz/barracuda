//! Portable Barracuda System application lifecycle.
//!
//! Target entries acquire their native hardware once, construct the selected
//! Target resources, and pass that resource bundle here. This crate contains
//! no concrete Platform or Board implementation.

#![no_std]

use barracuda_event_router::{RouterError, RpcLaneStorage};
use barracuda_system::{System, SystemCreateError};
use embassy_executor::Spawner;
use static_cell::ConstStaticCell;

const COMPONENTS: usize = 32;
const RPC_MESSAGE_BYTES: usize = 512;
const RPC_QUEUE_DEPTH: usize = 8;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<COMPONENTS, RPC_MESSAGE_BYTES, RPC_QUEUE_DEPTH>> =
    ConstStaticCell::new(RpcLaneStorage::new());

/// Failure while constructing or driving the portable System application.
#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    /// The process already consumed its one statically allocated RPC lane set.
    #[error("Barracuda System application was already started")]
    AlreadyStarted,
    /// System resource assignment, filesystem setup, or Plugin startup failed.
    #[error("failed to construct Barracuda System: {0}")]
    System(#[from] SystemCreateError),
    /// The running Event Router stopped with an error.
    #[error("Barracuda System stopped: {0}")]
    Router(#[from] RouterError),
}

/// Constructs and drives the Barracuda System using one selected Target.
///
/// # Errors
///
/// Returns an error if the application was already started, System creation
/// fails, or the Event Router terminates with an error.
pub async fn run(
    spawner: Spawner,
    resources: barracuda_target::Resources,
) -> Result<(), ApplicationError> {
    let lanes = RPC_LANES
        .try_take()
        .ok_or(ApplicationError::AlreadyStarted)?;
    let mut system = System::new(lanes, resources, spawner).await?;
    (&mut system).await?;
    Ok(())
}

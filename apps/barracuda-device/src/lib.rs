//! Portable embedded Barracuda System application.
//!
//! Target composition crates acquire hardware and construct the independently
//! selected Target resources. This crate consumes only that portable resource
//! bundle and owns the same [`barracuda_system::System`] lifecycle on every
//! embedded target.

#![no_std]

use barracuda_event_router::{RouterError, RpcLaneStorage};
use barracuda_system::{System, SystemConfig, SystemCreateError};
use embassy_executor::Spawner;
use static_cell::ConstStaticCell;

// The fixed System graph registers exactly ten Components. Keeping the limit
// explicit makes graph growth fail visibly instead of silently exhausting the
// embedded target's application heap.
const COMPONENTS: usize = 10;
// This is the minimum frame width accepted by every schema in the fixed graph.
const RPC_MESSAGE_BYTES: usize = 512;
const RPC_QUEUE_DEPTH: usize = 4;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<COMPONENTS, RPC_MESSAGE_BYTES, RPC_QUEUE_DEPTH>> =
    ConstStaticCell::new(RpcLaneStorage::new());

/// Failure while constructing or driving the portable embedded System.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    /// The application's one statically allocated Event Router lane set was already consumed.
    #[error("embedded System was already started")]
    AlreadyStarted,
    /// System resource assignment, filesystem setup, or Plugin startup failed.
    #[error("failed to construct embedded Barracuda System: {0}")]
    System(#[from] SystemCreateError),
    /// The running Event Router stopped with an error.
    #[error("embedded Barracuda System stopped: {0}")]
    Router(#[from] RouterError),
}

/// Constructs and drives the reusable embedded [`System`] lifecycle.
///
/// Target compositions call this function after their independently selected
/// Platform and Board HAL produce resources.
///
/// # Errors
///
/// Returns an error if this application was already started, System creation
/// fails, or the Event Router terminates with an error.
pub async fn run(
    spawner: Spawner,
    resources: barracuda_target::Resources,
) -> Result<(), DeviceError> {
    let lanes = RPC_LANES.try_take().ok_or(DeviceError::AlreadyStarted)?;
    let mut system =
        System::new_with_config(lanes, resources, spawner, SystemConfig::embedded()).await?;
    (&mut system).await?;
    Ok(())
}

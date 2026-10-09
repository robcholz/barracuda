//! Portable Barracuda System application lifecycle.
//!
//! Target entries acquire their native hardware once, construct the selected
//! Target resources, and pass that resource bundle here. This crate contains
//! no concrete Platform or Board implementation.

#![no_std]
#![recursion_limit = "256"]

use barracuda_system::{ReceiveBuffers, System, SystemCreateError};
use embassy_executor::Spawner;

/// Socket buffers of the receive slots, as many as the selected Platform
/// allows on any Board; none when it allows none.
static RECEIVE_BUFFERS: ReceiveBuffers<{ barracuda_target::RECEIVE_SLOTS }> = ReceiveBuffers::new();

/// Failure while constructing or driving the portable System application.
#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    /// System resource assignment, filesystem setup, or Plugin startup failed.
    #[error("failed to construct Barracuda System: {0}")]
    System(#[from] SystemCreateError),
}

/// Constructs and drives the Barracuda System using one selected Target.
///
/// # Errors
///
/// Returns an error if System creation fails.
pub async fn run(
    spawner: Spawner,
    resources: barracuda_target::Resources,
) -> Result<(), ApplicationError> {
    let system = System::new(
        resources,
        barracuda_target::TARGET_IDENTITY,
        RECEIVE_BUFFERS.take(),
        spawner,
    )
    .await?;
    core::future::pending::<()>().await;
    drop(system);
    Ok(())
}

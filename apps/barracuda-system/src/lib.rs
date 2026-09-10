//! Portable Barracuda System application lifecycle.
//!
//! Target entries acquire their native hardware once, construct the selected
//! Target resources, and pass that resource bundle here. This crate contains
//! no concrete Platform or Board implementation.

#![no_std]

use barracuda_system::{System, SystemCreateError};
use embassy_executor::Spawner;

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
    let system = System::new(resources, barracuda_target::TARGET_IDENTITY, spawner).await?;
    core::future::pending::<()>().await;
    drop(system);
    Ok(())
}

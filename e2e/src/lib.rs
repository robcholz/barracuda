//! Host-only composition support for full Barracuda System e2e tests.
//!
//! This crate is an application-level composition root. It keeps Platform and
//! Board HAL construction independent, then passes their typed resources to
//! [`barracuda_system::System`].

use barracuda_board_hal::BoardHal as _;
use barracuda_platform::{Partitions, Platform, PlatformResources};
use barracuda_target_api::TargetResources;
use barracuda_tls::PlaintextTls;
use embassy_executor::Spawner;

pub mod board;
pub mod mock_drivers;
mod network;
mod platform;

use board::E2eBoardHal;
use platform::PARTITION_CAPACITY;
pub use platform::{
    E2ePlatform, E2ePlatformBindings, E2ePlatformError, MemoryNorFlash, MemoryNorFlashError,
};

/// Complete concrete resource type passed from the e2e target to System.
pub type Resources = TargetResources<
    PlatformResources<PlaintextTls, Partitions<MemoryNorFlash, PARTITION_CAPACITY>>,
    <E2eBoardHal as barracuda_board_hal::BoardHal>::Resources,
>;

/// Constructs the two e2e target axes and preserves their ownership envelope.
///
/// # Errors
///
/// Returns a Platform or Board HAL initialization error.
pub async fn resources(spawner: Spawner) -> Result<Resources, E2eResourcesError> {
    E2ePlatform::prepare().map_err(E2eResourcesError::Platform)?;
    let board_hal = E2eBoardHal::initialize(spawner, ())
        .await
        .map_err(E2eResourcesError::BoardHal)?;
    let platform = E2ePlatform::initialize(spawner, E2ePlatformBindings::new())
        .await
        .map_err(E2eResourcesError::Platform)?;
    Ok(TargetResources {
        platform,
        board_hal,
    })
}

/// Failure while constructing the e2e target resource axes.
#[derive(Debug, thiserror::Error)]
pub enum E2eResourcesError {
    /// The deterministic Platform could not initialize.
    #[error(transparent)]
    Platform(#[from] E2ePlatformError),
    /// The deterministic Board HAL could not initialize.
    #[error(transparent)]
    BoardHal(#[from] core::convert::Infallible),
}

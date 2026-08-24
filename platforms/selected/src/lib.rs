//! Compile-time selected Board and Platform resource factory.
//!
//! Applications depend on this crate instead of parsing selection YAML or
//! importing a concrete Platform crate. The selected Platform remains
//! statically dispatched.

#![no_std]

use barracuda_platform::{Platform, PlatformResources};
use embassy_executor::Spawner;

mod selected {
    include!(concat!(env!("OUT_DIR"), "/selected_target.rs"));
}

use selected::{SelectedPlatform, BOARD};

/// Database region type produced by the selected Platform.
pub type DatabaseRegion = <SelectedPlatform as Platform>::DatabaseRegion;

/// Complete resource bundle produced for the selected target.
pub type Resources = PlatformResources<
    <SelectedPlatform as Platform>::Network,
    <SelectedPlatform as Platform>::FileSystem,
    DatabaseRegion,
    <SelectedPlatform as Platform>::ModelApiFactory,
>;

/// Initialization error produced by the selected Platform.
pub type Error = <SelectedPlatform as Platform>::Error;

/// Prepares and constructs resources for the independently selected Board and Platform.
///
/// # Errors
///
/// Returns the selected Platform's preparation or initialization error.
pub async fn resources(spawner: Spawner) -> Result<Resources, Error> {
    SelectedPlatform::prepare()?;
    SelectedPlatform::initialize(spawner, &BOARD).await
}

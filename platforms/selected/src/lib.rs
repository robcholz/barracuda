//! Independently selected Platform implementation.

#![no_std]

use barracuda_platform::{Platform, PlatformInitResult, PlatformResources};
use embassy_executor::Spawner;

mod selected {
    include!(concat!(env!("OUT_DIR"), "/selected_platform.rs"));
}

/// Independently selected concrete Platform.
pub use selected::SelectedPlatform;
/// Name of the independently selected Platform.
pub use selected::PLATFORM_NAME;

/// Board/HAL-produced bindings required by the selected Platform.
pub type Bindings = <SelectedPlatform as Platform>::Bindings;

/// Resource bundle produced only by the selected Platform.
pub type Resources = PlatformResources<<SelectedPlatform as Platform>::Partitions>;

/// Initialization error produced only by the selected Platform.
pub type Error = <SelectedPlatform as Platform>::Error;

/// Prepares the selected Platform runtime.
///
/// # Errors
///
/// Returns the selected Platform's runtime preparation error.
pub fn prepare() -> Result<(), Error> {
    SelectedPlatform::prepare()
}

/// Constructs only the selected Platform resources.
///
/// # Errors
///
/// Returns the selected Platform's initialization error.
pub async fn resources(
    spawner: Spawner,
    bindings: Bindings,
) -> PlatformInitResult<SelectedPlatform> {
    SelectedPlatform::initialize(spawner, bindings).await
}

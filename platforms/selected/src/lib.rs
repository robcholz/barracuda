//! Independently selected Platform implementation.

#![no_std]

use barracuda_platform::{Platform, PlatformInitResult, PlatformResources};
use embassy_executor::Spawner;

/// Non-runnable placeholder used only when compiling source-workspace tooling.
pub struct UnconfiguredPlatform;

/// Error returned if the non-runnable placeholder is initialized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnconfiguredPlatformError;

impl core::fmt::Display for UnconfiguredPlatformError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Barracuda target was not built through the generated build workspace")
    }
}

impl core::error::Error for UnconfiguredPlatformError {}

impl Platform for UnconfiguredPlatform {
    type Bindings = &'static ();
    type Tls = ();
    type Partitions = ();
    type Error = UnconfiguredPlatformError;

    fn prepare() -> Result<(), Self::Error> {
        Err(UnconfiguredPlatformError)
    }

    async fn initialize(_spawner: Spawner, _bindings: Self::Bindings) -> PlatformInitResult<Self> {
        Err(UnconfiguredPlatformError)
    }
}

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
pub type Resources = PlatformResources<
    <SelectedPlatform as Platform>::Tls,
    <SelectedPlatform as Platform>::Partitions,
>;

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

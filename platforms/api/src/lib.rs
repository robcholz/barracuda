//! Compile-time Platform API and initialization boundary.
//!
//! A concrete Platform owns runtime/resource initialization and permanent
//! driver tasks. The selected-target facade invokes it and returns the handles
//! consumed by System.

#![no_std]

use core::future::Future;

use barracuda_board::Board;
use barracuda_fs::FileSystem;
use embassy_executor::Spawner;
use embedded_storage_async::nor_flash::NorFlash;

/// Portable resources produced by one concrete Platform.
///
/// The network is shared for the process lifetime because network stacks and
/// their socket state are normally statically allocated. The Platform has
/// already validated the Board bindings against its native layout before
/// returning these handles.
pub struct PlatformResources<Network: 'static, Filesystem, DatabaseRegion, ModelApiFactory> {
    /// Initialized network handle. Its runner remains owned by a Platform task.
    pub network: &'static Network,
    /// Lightweight filesystem handle for System and Plugin consumers.
    pub filesystem: Filesystem,
    /// Board-isolated NOR region reserved for the system database.
    pub database_region: DatabaseRegion,
    /// Platform-configured constructor for Model API clients.
    pub model_api_factory: ModelApiFactory,
}

/// Result of initializing one statically selected [`Platform`].
pub type PlatformInitResult<P> = Result<
    PlatformResources<
        <P as Platform>::Network,
        <P as Platform>::FileSystem,
        <P as Platform>::DatabaseRegion,
        <P as Platform>::ModelApiFactory,
    >,
    <P as Platform>::Error,
>;

/// One statically selected Barracuda execution platform.
///
/// Implementations initialize concrete resources and spawn every permanent
/// Platform-owned task before returning. Host and device Platforms use the
/// same Embassy execution model. This trait is never used as a trait object;
/// associated types keep resource calls statically dispatched.
pub trait Platform: Sized + 'static {
    /// Shared platform network implementation.
    type Network: 'static;
    /// Cloneable platform filesystem handle.
    type FileSystem: FileSystem;
    /// Asynchronous NOR partition reserved for the system database.
    type DatabaseRegion: NorFlash;
    /// Platform-configured constructor for Model API clients.
    type ModelApiFactory: 'static;
    /// Platform initialization failure.
    type Error;

    /// Installs process-lifetime runtime support needed before any Platform
    /// capability is polled. Device Platforms normally use the default no-op;
    /// Host uses it to install its OS reactor behind the Embassy executor.
    /// The selected-target facade calls this immediately before
    /// [`Self::initialize`]; applications do not call it directly.
    fn prepare() -> Result<(), Self::Error> {
        Ok(())
    }

    /// Initializes the Platform for an independently selected Board and starts
    /// its permanent Embassy tasks.
    fn initialize(
        spawner: Spawner,
        board: &'static Board,
    ) -> impl Future<Output = PlatformInitResult<Self>>;
}

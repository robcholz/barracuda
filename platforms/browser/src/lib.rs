//! Browser Platform implementation for a dedicated Web Worker.

#![cfg_attr(not(target_os = "wasi"), allow(dead_code))]

#[cfg(target_os = "wasi")]
#[allow(unsafe_code)]
mod executor;
#[cfg(target_os = "wasi")]
#[allow(unsafe_code)]
mod ffi;
#[cfg(target_os = "wasi")]
mod flash;
#[cfg(target_os = "wasi")]
mod layout;
#[cfg(target_os = "wasi")]
pub mod network;
#[cfg(target_os = "wasi")]
mod time;

#[cfg(target_os = "wasi")]
mod implementation {
    use core::cell::RefCell;

    use crate::{
        flash::{OpfsNorFlash, OpfsNorFlashError},
        layout::{selected_layout, FileLayoutError, FileRegionAccess},
    };
    use barracuda_platform::{
        NamedPartition, PartitionAccess, Partitions, PartitionsInsertError, Platform,
        PlatformInitResult, PlatformResources,
    };
    use embassy_embedded_hal::flash::partition::BlockingPartition;
    use embassy_executor::Spawner;
    use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};

    const PARTITION_CAPACITY: usize = 16;

    struct BootConfig {
        system_image: Vec<u8>,
    }

    thread_local! {
        static BOOT_CONFIG: RefCell<Option<BootConfig>> = const { RefCell::new(None) };
    }

    /// One partition of the Browser Platform's OPFS flash image.
    pub type BrowserPartition = BlockingPartition<'static, CriticalSectionRawMutex, OpfsNorFlash>;
    /// Complete native partition collection produced by the Browser Platform.
    pub type BrowserPartitions = Partitions<BrowserPartition, PARTITION_CAPACITY>;

    /// Concrete Browser Platform implementation.
    pub struct BrowserPlatform;

    impl BrowserPlatform {
        async fn initialize_partitions() -> Result<BrowserPartitions, BrowserPlatformError> {
            let image = BOOT_CONFIG
                .with(|configured| configured.borrow_mut().take())
                .ok_or(BrowserPlatformError::NotConfigured)?
                .system_image;
            let layout = selected_layout();
            let regions = layout.validate(image.len())?;
            let mut flash = OpfsNorFlash::open("board.flash", layout.capacity()).await?;
            for region in regions {
                if region.access() != FileRegionAccess::ReadOnly {
                    continue;
                }
                let start = region.offset() as usize;
                let end = start
                    .checked_add(region.size() as usize)
                    .ok_or(FileLayoutError::Range)?;
                let provisioned = image.get(start..end).ok_or(FileLayoutError::Range)?;
                flash.replace_region(region.offset(), region.size() as usize, provisioned)?;
            }
            let flash = Box::leak(Box::new(Mutex::<CriticalSectionRawMutex, _>::new(
                RefCell::new(flash),
            )));

            let mut partitions = BrowserPartitions::new();
            for region in regions {
                let access = match region.access() {
                    FileRegionAccess::ReadOnly => PartitionAccess::ReadOnly,
                    FileRegionAccess::ReadWrite => PartitionAccess::ReadWrite,
                };
                partitions.insert(NamedPartition::new(
                    region.name(),
                    access,
                    region.filesystem(),
                    BlockingPartition::new(flash, region.offset(), region.size()),
                ))?;
            }
            Ok(partitions)
        }
    }

    impl Platform for BrowserPlatform {
        type Bindings = ();
        type Tls = barracuda_tls::PlaintextTls;
        type Partitions = BrowserPartitions;
        type Error = BrowserPlatformError;

        fn prepare() -> Result<(), Self::Error> {
            Ok(())
        }

        async fn initialize(spawner: Spawner, (): ()) -> PlatformInitResult<Self> {
            let partitions = Self::initialize_partitions().await?;
            let ip_stack = crate::network::create_stack(spawner)
                .await
                .map_err(BrowserPlatformError::Host)?;
            Ok(PlatformResources {
                ip_stack,
                tls: barracuda_tls::PlaintextTls,
                partitions,
            })
        }
    }

    /// Browser Platform initialization failure.
    #[derive(Debug, thiserror::Error)]
    pub enum BrowserPlatformError {
        /// The generated application entry was invoked without boot inputs.
        #[error("Browser Platform boot inputs are not configured")]
        NotConfigured,
        /// A downloaded native flash image has the wrong size.
        #[error("Browser flash image is {actual} bytes, expected {expected}")]
        ImageSize {
            /// Required partition size.
            expected: usize,
            /// Downloaded byte length.
            actual: usize,
        },
        /// OPFS initialization or access failed.
        #[error(transparent)]
        Flash(#[from] OpfsNorFlashError),
        /// The selected Board's native file layout is invalid.
        #[error(transparent)]
        Layout(#[from] FileLayoutError),
        /// The native partition collection rejected an entry.
        #[error("Browser native partition collection rejected an entry: {0:?}")]
        Partitions(#[from] PartitionsInsertError),
        /// A browser API operation failed.
        #[error("Browser host operation failed: {0}")]
        Host(String),
    }

    #[doc(hidden)]
    pub fn configure_boot() -> Result<(), BrowserPlatformError> {
        let system_image = crate::ffi::system_image()
            .map_err(|error| BrowserPlatformError::Host(error.to_string()))?;
        let actual = system_image.len();
        let expected = selected_layout().capacity();
        if actual != expected {
            return Err(BrowserPlatformError::ImageSize { expected, actual });
        }
        BOOT_CONFIG.with(|configured| {
            let mut configured = configured.borrow_mut();
            if configured.is_some() {
                return Err(BrowserPlatformError::Host(String::from(
                    "Browser Platform is already starting",
                )));
            }
            configured.replace(BootConfig { system_image });
            Ok(())
        })
    }

    #[doc(hidden)]
    pub fn report_fatal(message: &str) {
        crate::ffi::report_error(message);
    }
}

#[cfg(target_os = "wasi")]
pub use implementation::{
    BrowserPartition, BrowserPartitions, BrowserPlatform, BrowserPlatformError,
};

#[cfg(not(target_os = "wasi"))]
/// Browser Platform marker available to host-side catalog tooling.
pub struct BrowserPlatform;

/// Constructs the Browser Platform's unit binding without inspecting the Board.
#[doc(hidden)]
#[macro_export]
macro_rules! platform_bindings {
    ($_board:expr) => {
        ()
    };
}

#[cfg(target_os = "wasi")]
#[doc(hidden)]
pub use executor::start as __start_executor;
#[cfg(target_os = "wasi")]
#[doc(hidden)]
pub use ffi::report_started as __report_started;
#[cfg(target_os = "wasi")]
#[doc(hidden)]
pub use implementation::{configure_boot as __configure_boot, report_fatal as __report_fatal};
#[cfg(target_os = "wasi")]
#[doc(hidden)]
pub use time::dispatch as __dispatch_alarm;

/// Generates the Web Worker entry around one Barracuda application future.
#[cfg(target_os = "wasi")]
#[macro_export]
macro_rules! platform_entry {
    (|$spawner:ident| $application:expr) => {
        extern crate alloc;
        extern crate std;

        #[embassy_executor::task]
        async fn __browser_application($spawner: embassy_executor::Spawner) {
            if let Err(error) = $application.await {
                $crate::__report_fatal(&alloc::format!("{error}"));
            }
        }

        #[unsafe(no_mangle)]
        #[doc(hidden)]
        pub extern "C" fn barracuda_browser_start() {
            if let Err(error) = $crate::__configure_boot() {
                $crate::__report_fatal(&alloc::format!("{error}"));
                return;
            }
            if let Err(error) = $crate::__start_executor(|spawner| {
                spawner.must_spawn(__browser_application(spawner));
            }) {
                $crate::__report_fatal(&alloc::format!("{error}"));
                return;
            }
            $crate::__report_started();
        }

        #[unsafe(no_mangle)]
        #[doc(hidden)]
        pub extern "C" fn barracuda_browser_alarm() {
            $crate::__dispatch_alarm();
        }
    };
}

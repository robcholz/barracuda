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
pub mod network;
#[cfg(target_os = "wasi")]
mod time;

#[cfg(target_os = "wasi")]
mod implementation {
    use core::cell::RefCell;

    use crate::flash::{OpfsNorFlash, OpfsNorFlashError};
    use barracuda_board::Board;
    use barracuda_platform::{
        NamedPartition, PartitionAccess, PartitionFilesystem, Partitions, PartitionsInsertError,
        Platform, PlatformInitResult, PlatformResources,
    };
    use embassy_embedded_hal::flash::partition::BlockingPartition;
    use embassy_executor::Spawner;
    use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};

    const FLASH_CAPACITY: usize = 4 * 1024 * 1024;
    const SYSTEM_OFFSET: u32 = 0;
    const SYSTEM_SIZE: u32 = 2 * 1024 * 1024;
    const RESOURCES_OFFSET: u32 = SYSTEM_OFFSET + SYSTEM_SIZE;
    const RESOURCES_SIZE: u32 = 1024 * 1024;
    const DATABASE_OFFSET: u32 = RESOURCES_OFFSET + RESOURCES_SIZE;
    const DATABASE_SIZE: u32 = 1024 * 1024;
    const PARTITION_CAPACITY: usize = 3;

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
            let resources_end = usize::try_from(RESOURCES_OFFSET)
                .ok()
                .and_then(|offset| offset.checked_add(RESOURCES_SIZE as usize))
                .ok_or(BrowserPlatformError::ImageSize {
                    expected: FLASH_CAPACITY,
                    actual: image.len(),
                })?;
            let resources = image.get(RESOURCES_OFFSET as usize..resources_end).ok_or(
                BrowserPlatformError::ImageSize {
                    expected: FLASH_CAPACITY,
                    actual: image.len(),
                },
            )?;
            let mut flash = OpfsNorFlash::open("board.flash", FLASH_CAPACITY).await?;
            flash.replace_region(RESOURCES_OFFSET, RESOURCES_SIZE as usize, resources)?;
            let flash = Box::leak(Box::new(Mutex::<CriticalSectionRawMutex, _>::new(
                RefCell::new(flash),
            )));

            let mut partitions = BrowserPartitions::new();
            partitions.insert(NamedPartition::new(
                "system",
                PartitionAccess::ReadWrite,
                PartitionFilesystem::LittleFs,
                BlockingPartition::new(flash, SYSTEM_OFFSET, SYSTEM_SIZE),
            ))?;
            partitions.insert(NamedPartition::new(
                "resources",
                PartitionAccess::ReadOnly,
                PartitionFilesystem::FatFs,
                BlockingPartition::new(flash, RESOURCES_OFFSET, RESOURCES_SIZE),
            ))?;
            partitions.insert(NamedPartition::new(
                "kv_database",
                PartitionAccess::ReadWrite,
                PartitionFilesystem::Raw,
                BlockingPartition::new(flash, DATABASE_OFFSET, DATABASE_SIZE),
            ))?;
            Ok(partitions)
        }
    }

    impl Platform for BrowserPlatform {
        type Bindings = &'static Board;
        type Tls = barracuda_tls::PlaintextTls;
        type Partitions = BrowserPartitions;
        type Error = BrowserPlatformError;

        fn prepare() -> Result<(), Self::Error> {
            Ok(())
        }

        async fn initialize(spawner: Spawner, board: &'static Board) -> PlatformInitResult<Self> {
            if board.hardware().chip() != "browser" {
                return Err(BrowserPlatformError::IncompatibleChip {
                    chip: board.hardware().chip(),
                });
            }
            let partitions = Self::initialize_partitions().await?;
            let ip_stack = crate::network::create_stack(spawner)
                .await
                .map_err(BrowserPlatformError::Host)?;
            crate::ffi::report_device_url("./");
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
        /// The selected Board does not target the Browser Platform.
        #[error("Browser Platform does not support Board chip `{chip}`")]
        IncompatibleChip {
            /// Canonical chip name declared by the Board.
            chip: &'static str,
        },
        /// The generated application entry was invoked without boot inputs.
        #[error("Browser Platform boot inputs are not configured")]
        NotConfigured,
        /// A downloaded resource image has the wrong size.
        #[error("Browser resource image is {actual} bytes, expected {expected}")]
        ImageSize {
            /// Required partition size.
            expected: usize,
            /// Downloaded byte length.
            actual: usize,
        },
        /// OPFS initialization or access failed.
        #[error(transparent)]
        Flash(#[from] OpfsNorFlashError),
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
        if actual != FLASH_CAPACITY {
            return Err(BrowserPlatformError::ImageSize {
                expected: FLASH_CAPACITY,
                actual,
            });
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

//! Browser Platform implementation for a dedicated Web Worker.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

#[cfg(target_arch = "wasm32")]
mod flash;
#[cfg(target_arch = "wasm32")]
pub mod network;
#[cfg(target_arch = "wasm32")]
mod time;

#[cfg(target_arch = "wasm32")]
mod implementation {
    use core::cell::RefCell;

    use barracuda_board::Board;
    use barracuda_platform::{
        NamedPartition, PartitionAccess, PartitionFilesystem, Partitions, PartitionsInsertError,
        Platform, PlatformInitResult, PlatformResources,
    };
    use embassy_embedded_hal::flash::partition::BlockingPartition;
    use embassy_executor::Spawner;
    use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
    use wasm_bindgen::{JsCast as _, JsValue};

    use crate::flash::{OpfsNorFlash, OpfsNorFlashError};

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
            js_sys::global()
                .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
                .map(|_| ())
                .map_err(|_| BrowserPlatformError::NotDedicatedWorker)
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
                .map_err(BrowserPlatformError::javascript)?;
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
        /// Startup was attempted outside a dedicated worker.
        #[error("Browser Platform must run in a dedicated Web Worker")]
        NotDedicatedWorker,
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
        #[error("Browser API operation failed: {0}")]
        Javascript(String),
    }

    impl BrowserPlatformError {
        fn javascript(value: JsValue) -> Self {
            Self::Javascript(
                value
                    .as_string()
                    .unwrap_or_else(|| String::from("unknown JavaScript exception")),
            )
        }
    }

    #[doc(hidden)]
    pub fn configure_boot(
        gateway_url: String,
        system_image: js_sys::Uint8Array,
    ) -> Result<(), JsValue> {
        js_sys::global()
            .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
            .map_err(|_| JsValue::from_str("Barracuda must run in a dedicated Web Worker"))?;
        let actual = system_image.length() as usize;
        if actual != FLASH_CAPACITY {
            return Err(JsValue::from_str(
                &BrowserPlatformError::ImageSize {
                    expected: FLASH_CAPACITY,
                    actual,
                }
                .to_string(),
            ));
        }
        crate::network::configure_gateway(gateway_url)?;
        BOOT_CONFIG.with(|configured| {
            let mut configured = configured.borrow_mut();
            if configured.is_some() {
                return Err(JsValue::from_str("Browser Platform is already starting"));
            }
            configured.replace(BootConfig {
                system_image: system_image.to_vec(),
            });
            Ok(())
        })
    }

    #[doc(hidden)]
    pub fn report_fatal(message: &str) {
        if let Ok(worker) = js_sys::global().dyn_into::<web_sys::DedicatedWorkerGlobalScope>() {
            let event = js_sys::Object::new();
            let _ = js_sys::Reflect::set(
                &event,
                &JsValue::from_str("type"),
                &JsValue::from_str("error"),
            );
            let _ = js_sys::Reflect::set(
                &event,
                &JsValue::from_str("message"),
                &JsValue::from_str(message),
            );
            let _ = worker.post_message(&event);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use implementation::{
    BrowserPartition, BrowserPartitions, BrowserPlatform, BrowserPlatformError,
};

#[cfg(not(target_arch = "wasm32"))]
/// Browser Platform marker available to host-side catalog tooling.
pub struct BrowserPlatform;

#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub use implementation::{configure_boot as __configure_boot, report_fatal as __report_fatal};
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub use js_sys as __js_sys;
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub use wasm_bindgen as __wasm_bindgen;

/// Generates the Web Worker entry around one Barracuda application future.
#[cfg(target_arch = "wasm32")]
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

        use $crate::__wasm_bindgen as wasm_bindgen;

        #[$crate::__wasm_bindgen::prelude::wasm_bindgen]
        #[doc(hidden)]
        pub fn start(
            gateway_url: std::string::String,
            system_image: $crate::__wasm_bindgen::JsValue,
        ) -> Result<(), $crate::__wasm_bindgen::JsValue> {
            use $crate::__wasm_bindgen::JsCast as _;

            let system_image = system_image.dyn_into::<$crate::__js_sys::Uint8Array>()?;
            $crate::__configure_boot(gateway_url, system_image)?;
            let executor =
                std::boxed::Box::leak(std::boxed::Box::new(embassy_executor::Executor::new()));
            executor.start(|spawner| {
                spawner.must_spawn(__browser_application(spawner));
            });
            Ok(())
        }
    };
}

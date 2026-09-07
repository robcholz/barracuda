//! macOS Platform initialization from an independently selected Board.

use core::cell::RefCell;
use std::path::Path;

use barracuda_board::Board;
use barracuda_platform::{
    NamedPartition, PartitionAccess, Partitions, PartitionsInsertError, Platform,
    PlatformInitResult, PlatformResources,
};
use embassy_embedded_hal::flash::partition::BlockingPartition;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};

use crate::{
    FileLayout, FileLayoutError, FileNorFlash, FileNorFlashError, FileRegionAccess,
    MacosNetworkError,
};

const PARTITION_CAPACITY: usize = 16;

/// macOS paths baked from `platforms/macos/platform.yml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacosSettings {
    state_directory: &'static str,
    flash_image: &'static str,
}

impl MacosSettings {
    /// Creates macOS Platform settings.
    #[must_use]
    pub const fn new(state_directory: &'static str, flash_image: &'static str) -> Self {
        Self {
            state_directory,
            flash_image,
        }
    }

    /// Returns the root used for file-backed persistent state.
    #[must_use]
    pub const fn state_directory(&self) -> &'static str {
        self.state_directory
    }

    /// Returns the physical NOR image name within the state directory.
    #[must_use]
    pub const fn flash_image(&self) -> &'static str {
        self.flash_image
    }
}

/// One macOS file-backed native partition.
pub type MacosPartition = BlockingPartition<'static, CriticalSectionRawMutex, FileNorFlash>;
/// Complete arbitrary native partition collection produced by macOS.
pub type MacosPartitions = Partitions<MacosPartition, PARTITION_CAPACITY>;

/// Concrete macOS Platform implementation.
pub struct MacosPlatform;

impl MacosPlatform {
    /// Installs the macOS reactor used by UTUN I/O on the Embassy executor thread.
    ///
    /// # Errors
    /// Returns an error when the Tokio reactor threads cannot be created.
    pub fn install_reactor() -> Result<(), MacosPlatformError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Ok(());
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(MacosPlatformError::Runtime)?;
        let runtime = Box::leak(Box::new(runtime));
        let guard = runtime.enter();
        core::mem::forget(guard);
        Ok(())
    }

    /// Loads macOS trust roots and initializes the Platform TLS capability.
    ///
    /// # Errors
    /// Returns an error when the macOS trust store or TLS engine is invalid.
    pub fn initialize_tls() -> Result<barracuda_tls::MbedTls, MacosPlatformError> {
        crate::tls::initialize().map_err(MacosPlatformError::Tls)
    }

    /// Initializes partitions using generated macOS settings.
    ///
    /// # Errors
    /// Returns an error when the Board or native image layout is incompatible.
    pub async fn initialize_partitions_with_settings(
        board: &'static Board,
        settings: &MacosSettings,
    ) -> Result<MacosPartitions, MacosPlatformError> {
        Self::initialize_partitions_with_layout(board, settings, &crate::BOARD_FILE_LAYOUT).await
    }

    /// Initializes arbitrary named partitions from an explicit file-backed layout.
    ///
    /// # Errors
    /// Returns an error when the image or any native partition is invalid.
    pub async fn initialize_partitions_with_layout(
        board: &'static Board,
        settings: &MacosSettings,
        layout: &FileLayout,
    ) -> Result<MacosPartitions, MacosPlatformError> {
        if board.hardware().chip() != "macos" {
            return Err(MacosPlatformError::IncompatibleChip {
                chip: board.hardware().chip(),
            });
        }
        let state_directory = Path::new(settings.state_directory());
        let physical_flash = FileNorFlash::open(
            state_directory.join(settings.flash_image()),
            layout.capacity(),
        )?;
        let regions = layout.regions(&physical_flash)?;
        let physical_flash = Box::leak(Box::new(Mutex::<CriticalSectionRawMutex, _>::new(
            RefCell::new(physical_flash),
        )));
        let mut partitions = MacosPartitions::new();
        for region in regions {
            let access = match region.access() {
                FileRegionAccess::ReadOnly => PartitionAccess::ReadOnly,
                FileRegionAccess::ReadWrite => PartitionAccess::ReadWrite,
            };
            partitions.insert(NamedPartition::new(
                region.name(),
                access,
                region.filesystem(),
                BlockingPartition::new(physical_flash, region.offset(), region.size()),
            ))?;
        }
        Ok(partitions)
    }
}

impl Platform for MacosPlatform {
    type Bindings = &'static Board;
    type Tls = barracuda_tls::MbedTls;
    type Partitions = MacosPartitions;
    type Error = MacosPlatformError;

    fn prepare() -> Result<(), Self::Error> {
        crate::logging::install();
        log::info!("preparing macOS Platform");
        Self::install_reactor()
    }

    async fn initialize(spawner: Spawner, board: &'static Board) -> PlatformInitResult<Self> {
        log::info!("initializing macOS Platform partitions");
        let partitions =
            Self::initialize_partitions_with_settings(board, &crate::PLATFORM_SETTINGS).await?;
        log::info!("initializing macOS Platform network");
        let ip_stack = crate::network::initialize(spawner).await?;
        log::info!("initializing macOS Platform TLS");
        let tls = Self::initialize_tls()?;
        log::info!("initialized macOS Platform");
        Ok(PlatformResources {
            ip_stack,
            tls,
            partitions,
        })
    }
}

/// macOS Platform initialization failure.
#[derive(Debug, thiserror::Error)]
pub enum MacosPlatformError {
    /// The selected Board is not a macOS simulation target.
    #[error("macOS Platform does not support Board chip `{chip}`")]
    IncompatibleChip {
        /// Canonical chip name declared by the Board.
        chip: &'static str,
    },
    /// Tokio reactor initialization failed.
    #[error("failed to initialize macOS reactor: {0}")]
    Runtime(std::io::Error),
    /// The real UTUN-backed Embassy network failed to initialize.
    #[error(transparent)]
    Network(#[from] MacosNetworkError),
    /// Host TLS initialization failed.
    #[error(transparent)]
    Tls(#[from] crate::MacosTlsError),
    /// File-backed NOR initialization failed.
    #[error(transparent)]
    Flash(#[from] FileNorFlashError),
    /// Board layout is invalid.
    #[error(transparent)]
    Layout(#[from] FileLayoutError),
    /// The fixed-capacity partition collection could not accept the native layout.
    #[error("macOS native partition collection rejected an entry: {0:?}")]
    Partitions(#[from] PartitionsInsertError),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use barracuda_platform::Platform as _;

    use super::MacosPlatform;

    #[test]
    fn prepare_installs_the_global_log_backend_before_platform_initialization() {
        MacosPlatform::prepare().expect("prepare macOS Platform");

        assert_eq!(log::max_level(), crate::PLATFORM_LOG_LEVEL);
        assert!(log::log_enabled!(
            target: "barracuda_platform_macos",
            log::Level::Info
        ));
    }
}

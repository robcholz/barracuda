//! Linux Platform initialization from an independently selected Board.

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
    LinuxNetworkError,
};

const PARTITION_CAPACITY: usize = 16;

/// Linux settings baked from `platforms/linux/platform.yml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinuxSettings {
    state_directory: &'static str,
    flash_image: &'static str,
    network_interface: &'static str,
}

impl LinuxSettings {
    /// Creates Linux Platform settings.
    #[must_use]
    pub const fn new(
        state_directory: &'static str,
        flash_image: &'static str,
        network_interface: &'static str,
    ) -> Self {
        Self {
            state_directory,
            flash_image,
            network_interface,
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

    /// Returns the provisioned Linux TUN interface name.
    #[must_use]
    pub const fn network_interface(&self) -> &'static str {
        self.network_interface
    }
}

/// One Linux file-backed native partition.
pub type LinuxPartition = BlockingPartition<'static, CriticalSectionRawMutex, FileNorFlash>;
/// Complete arbitrary native partition collection produced by Linux.
pub type LinuxPartitions = Partitions<LinuxPartition, PARTITION_CAPACITY>;

/// Concrete Linux Platform implementation.
pub struct LinuxPlatform;

impl LinuxPlatform {
    /// Installs the Linux reactor used by TUN I/O on the Embassy executor thread.
    ///
    /// # Errors
    /// Returns an error when the Tokio reactor threads cannot be created.
    pub fn install_reactor() -> Result<(), LinuxPlatformError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Ok(());
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(LinuxPlatformError::Runtime)?;
        let runtime = Box::leak(Box::new(runtime));
        let guard = runtime.enter();
        core::mem::forget(guard);
        Ok(())
    }

    /// Loads Linux trust roots and initializes the Platform TLS capability.
    ///
    /// # Errors
    /// Returns an error when the Linux trust store or TLS engine is invalid.
    pub fn initialize_tls() -> Result<barracuda_tls::MbedTls, LinuxPlatformError> {
        crate::tls::initialize().map_err(LinuxPlatformError::Tls)
    }

    /// Initializes partitions using generated Linux settings.
    ///
    /// # Errors
    /// Returns an error when the Board or native image layout is incompatible.
    pub async fn initialize_partitions_with_settings(
        board: &'static Board,
        settings: &LinuxSettings,
    ) -> Result<LinuxPartitions, LinuxPlatformError> {
        Self::initialize_partitions_with_layout(board, settings, &crate::BOARD_FILE_LAYOUT).await
    }

    /// Initializes arbitrary named partitions from an explicit file-backed layout.
    ///
    /// # Errors
    /// Returns an error when the image or any native partition is invalid.
    pub async fn initialize_partitions_with_layout(
        board: &'static Board,
        settings: &LinuxSettings,
        layout: &FileLayout,
    ) -> Result<LinuxPartitions, LinuxPlatformError> {
        if board.hardware().chip() != "linux" {
            return Err(LinuxPlatformError::IncompatibleChip {
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
        let mut partitions = LinuxPartitions::new();
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

impl Platform for LinuxPlatform {
    type Bindings = &'static Board;
    type Tls = barracuda_tls::MbedTls;
    type Wifi = barracuda_platform::HostWifiDevice;
    type Partitions = LinuxPartitions;
    type Error = LinuxPlatformError;

    fn prepare() -> Result<(), Self::Error> {
        barracuda_bulk_memory::platform::install_global();
        crate::logging::install();
        log::info!("preparing Linux Platform");
        Self::install_reactor()
    }

    async fn initialize(spawner: Spawner, board: &'static Board) -> PlatformInitResult<Self> {
        log::info!("initializing Linux Platform partitions");
        let partitions =
            Self::initialize_partitions_with_settings(board, &crate::PLATFORM_SETTINGS).await?;
        log::info!("initializing Linux Platform network");
        let ip_stack =
            crate::network::initialize(spawner, crate::PLATFORM_SETTINGS.network_interface())
                .await?;
        log::info!("initializing Linux Platform TLS");
        let tls = Self::initialize_tls()?;
        log::info!("initialized Linux Platform");
        Ok(PlatformResources {
            ip_stack,
            wifi: barracuda_platform::HostWifiDevice::new(ip_stack),
            tls,
            partitions,
        })
    }
}

/// Linux Platform initialization failure.
#[derive(Debug, thiserror::Error)]
pub enum LinuxPlatformError {
    /// The selected Board is not a Linux simulation target.
    #[error("Linux Platform does not support Board chip `{chip}`")]
    IncompatibleChip {
        /// Canonical chip name declared by the Board.
        chip: &'static str,
    },
    /// Tokio reactor initialization failed.
    #[error("failed to initialize Linux reactor: {0}")]
    Runtime(std::io::Error),
    /// The real TUN-backed Embassy network failed to initialize.
    #[error(transparent)]
    Network(#[from] LinuxNetworkError),
    /// Host TLS initialization failed.
    #[error(transparent)]
    Tls(#[from] crate::LinuxTlsError),
    /// File-backed NOR initialization failed.
    #[error(transparent)]
    Flash(#[from] FileNorFlashError),
    /// Board layout is invalid.
    #[error(transparent)]
    Layout(#[from] FileLayoutError),
    /// The fixed-capacity partition collection could not accept the native layout.
    #[error("Linux native partition collection rejected an entry: {0:?}")]
    Partitions(#[from] PartitionsInsertError),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use barracuda_platform::Platform as _;

    use super::LinuxPlatform;

    #[test]
    fn prepare_installs_the_global_log_backend_before_platform_initialization() {
        LinuxPlatform::prepare().expect("prepare Linux Platform");

        assert_eq!(log::max_level(), crate::PLATFORM_LOG_LEVEL);
        assert!(log::log_enabled!(
            target: "barracuda_platform_linux",
            log::Level::Info
        ));
    }
}

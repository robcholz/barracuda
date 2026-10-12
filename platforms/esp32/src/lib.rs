//! ESP32 Platform mechanisms backed by an ESP-IDF native partition table.

#![no_std]

/// Platform-owned GPIO, SPI, I2C, and delay implementation.
#[cfg(target_arch = "xtensa")]
pub mod hal;

#[cfg(target_arch = "xtensa")]
mod entropy;

#[cfg(target_arch = "xtensa")]
pub use entropy::Esp32Entropy;

#[cfg(target_arch = "xtensa")]
#[doc(hidden)]
pub mod application;

#[cfg(target_arch = "xtensa")]
mod wifi {
    include!("../../esp32/src/wifi.rs");
}

#[cfg(target_arch = "xtensa")]
pub use wifi::{EspWifiDevice as Esp32WifiDevice, EspWifiError as Esp32WifiError};

/// Runtime access discipline declared by ESP-IDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Esp32RegionAccess {
    /// Provisioned bytes cannot be changed at runtime.
    ReadOnly,
    /// Runtime code may erase and program the region.
    ReadWrite,
}

/// One Board-bound region from the ESP-IDF partition table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Esp32Region {
    name: &'static str,
    offset: u32,
    size: u32,
    access: Esp32RegionAccess,
    filesystem: barracuda_platform::PartitionFilesystem,
}

impl Esp32Region {
    /// Creates a generated native ESP32 region.
    #[must_use]
    pub const fn new(
        name: &'static str,
        offset: u32,
        size: u32,
        access: Esp32RegionAccess,
        filesystem: barracuda_platform::PartitionFilesystem,
    ) -> Self {
        Self {
            name,
            offset,
            size,
            access,
            filesystem,
        }
    }

    /// Returns the ESP-IDF partition label.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the flash-relative byte offset.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Returns the region size in bytes.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// Returns the ESP-IDF runtime access flag.
    #[must_use]
    pub const fn access(&self) -> Esp32RegionAccess {
        self.access
    }

    /// Returns the filesystem declared by the ESP-IDF partition subtype.
    #[must_use]
    pub const fn filesystem(&self) -> barracuda_platform::PartitionFilesystem {
        self.filesystem
    }
}

/// Complete native ESP-IDF partition table selected by the Board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Esp32PartitionTable {
    chip: &'static str,
    ota_slot_count: usize,
    regions: &'static [Esp32Region],
}

impl Esp32PartitionTable {
    /// Creates a build-validated projection of a complete ESP-IDF table.
    #[must_use]
    pub const fn new(
        chip: &'static str,
        ota_slot_count: usize,
        regions: &'static [Esp32Region],
    ) -> Self {
        Self {
            chip,
            ota_slot_count,
            regions,
        }
    }

    /// Returns the selected ESP HAL chip.
    #[must_use]
    pub const fn chip(&self) -> &'static str {
        self.chip
    }

    /// Returns the number of OTA application slots in the native table.
    #[must_use]
    pub const fn ota_slot_count(&self) -> usize {
        self.ota_slot_count
    }

    /// Returns every entry from the native table in native order.
    #[must_use]
    pub const fn regions(&self) -> &'static [Esp32Region] {
        self.regions
    }

    /// Finds a region by its native ESP-IDF label.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Esp32Region> {
        self.regions.iter().find(|region| region.name() == name)
    }
}

include!(concat!(env!("OUT_DIR"), "/esp32_layout.rs"));

#[cfg(target_arch = "xtensa")]
mod internal_flash {
    use core::cell::RefCell;

    use barracuda_board::Board;
    use embassy_embedded_hal::flash::partition::BlockingPartition;
    use embassy_executor::Spawner;
    use embassy_net::Stack;
    use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};

    use barracuda_platform::{
        NamedPartition, PartitionAccess, Partitions, PartitionsInsertError, Platform,
        PlatformInitResult, PlatformResources,
    };

    use crate::{Esp32RegionAccess, BOARD_ESP32_PARTITION_TABLE};

    /// ESP HAL synchronous internal flash driver.
    pub type Esp32Flash<'d> = esp_storage::FlashStorage<'d>;

    /// One ESP-IDF partition backed by ESP HAL flash.
    pub type Esp32Partition =
        BlockingPartition<'static, CriticalSectionRawMutex, Esp32Flash<'static>>;

    /// Generic named ESP-IDF partitions available to System.
    pub type Esp32Partitions = Partitions<Esp32Partition, 16>;

    /// ESP Platform implementation for the selected ESP32 target.
    pub struct Esp32Platform;

    /// Board/HAL bindings consumed by [`Esp32Platform`].
    pub struct Esp32PlatformBindings {
        ip_stack: Stack<'static>,
        wifi: crate::Esp32WifiDevice,
        flash: &'static Mutex<CriticalSectionRawMutex, RefCell<Esp32Flash<'static>>>,
    }

    impl Esp32PlatformBindings {
        /// Binds initialized ESP Platform services to a compatible Board.
        ///
        /// The IP runner must already be owned and running inside the ESP
        /// Platform network mechanism. The selected Target supplies the flash
        /// peripheral from its Board/HAL composition.
        ///
        /// # Errors
        ///
        /// Returns an error when the selected Board does not use ESP32.
        pub fn from_initialized_services(
            board: &Board,
            ip_stack: Stack<'static>,
            wifi: crate::Esp32WifiDevice,
            flash: &'static Mutex<CriticalSectionRawMutex, RefCell<Esp32Flash<'static>>>,
        ) -> Result<Self, Esp32PlatformError> {
            if board.hardware().chip() != "esp32" {
                return Err(Esp32PlatformError::IncompatibleChip);
            }
            Ok(Self {
                ip_stack,
                wifi,
                flash,
            })
        }
    }

    /// Turns the ESP HAL flash peripheral into the shared synchronous flash backend.
    #[must_use]
    pub fn flash<'d>(peripheral: esp_hal::peripherals::FLASH<'d>) -> Esp32Flash<'d> {
        esp_storage::FlashStorage::new(peripheral)
    }

    /// Projects every build-validated native entry from shared ESP flash.
    ///
    /// # Errors
    ///
    /// Returns an error if the Platform collection capacity is smaller than
    /// the selected ESP-IDF table.
    pub fn partitions(
        flash: &'static Mutex<CriticalSectionRawMutex, RefCell<Esp32Flash<'static>>>,
    ) -> Result<Esp32Partitions, barracuda_platform::PartitionsInsertError> {
        let mut partitions = Esp32Partitions::new();
        for region in BOARD_ESP32_PARTITION_TABLE.regions() {
            let access = match region.access() {
                Esp32RegionAccess::ReadOnly => PartitionAccess::ReadOnly,
                Esp32RegionAccess::ReadWrite => PartitionAccess::ReadWrite,
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

    impl Platform for Esp32Platform {
        type Bindings = Esp32PlatformBindings;
        type Wifi = crate::Esp32WifiDevice;
        type Entropy = crate::Esp32Entropy;
        type Partitions = Esp32Partitions;
        type Error = Esp32PlatformError;

        fn prepare() -> Result<(), Self::Error> {
            esp_println::logger::init_logger(crate::PLATFORM_LOG_LEVEL);
            log::info!("preparing ESP32 Platform");
            Ok(())
        }

        async fn initialize(
            _spawner: Spawner,
            bindings: Self::Bindings,
        ) -> PlatformInitResult<Self> {
            log::info!("initializing ESP32 Platform partitions");
            let partitions = partitions(bindings.flash)?;
            log::info!("initialized ESP32 Platform");
            let ip_stack = bindings.ip_stack;
            Ok(PlatformResources {
                ip_stack,
                wifi: bindings.wifi,
                entropy: crate::Esp32Entropy,
                partitions,
            })
        }
    }

    /// ESP32 Platform initialization failure.
    #[derive(Debug)]
    pub enum Esp32PlatformError {
        /// The selected Board targets a different chip family.
        IncompatibleChip,
        /// The native table exceeded or violated the generic collection.
        Partitions(PartitionsInsertError),
        /// ESP radio initialization failed.
        Wifi(crate::Esp32WifiError),
        /// An Embassy network runner could not be allocated.
        NetworkTask,
    }

    impl From<PartitionsInsertError> for Esp32PlatformError {
        fn from(error: PartitionsInsertError) -> Self {
            Self::Partitions(error)
        }
    }

    impl core::fmt::Display for Esp32PlatformError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::IncompatibleChip => formatter.write_str("incompatible ESP32 chip"),
                Self::Partitions(error) => write!(formatter, "invalid ESP32 partitions: {error}"),
                Self::Wifi(error) => write!(formatter, "failed to initialize ESP32 Wi-Fi: {error}"),
                Self::NetworkTask => formatter.write_str("failed to allocate ESP32 network task"),
            }
        }
    }

    impl core::error::Error for Esp32PlatformError {}

    impl From<crate::Esp32WifiError> for Esp32PlatformError {
        fn from(error: crate::Esp32WifiError) -> Self {
            Self::Wifi(error)
        }
    }
}

#[cfg(target_arch = "xtensa")]
pub use internal_flash::{
    flash, partitions, Esp32Flash, Esp32Partition, Esp32Partitions, Esp32Platform,
    Esp32PlatformBindings, Esp32PlatformError,
};

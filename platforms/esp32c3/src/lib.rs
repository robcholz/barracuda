//! ESP32-C3 Platform mechanisms backed by an ESP-IDF native partition table.

#![no_std]

/// Platform-owned GPIO, SPI, I2C, and delay implementation.
#[cfg(target_arch = "riscv32")]
pub mod hal;

/// Runtime access discipline declared by ESP-IDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Esp32C3RegionAccess {
    /// Provisioned bytes cannot be changed at runtime.
    ReadOnly,
    /// Runtime code may erase and program the region.
    ReadWrite,
}

/// One Board-bound region from the ESP-IDF partition table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Esp32C3Region {
    name: &'static str,
    offset: u32,
    size: u32,
    access: Esp32C3RegionAccess,
    filesystem: barracuda_platform::PartitionFilesystem,
}

impl Esp32C3Region {
    /// Creates a generated native ESP32 region.
    #[must_use]
    pub const fn new(
        name: &'static str,
        offset: u32,
        size: u32,
        access: Esp32C3RegionAccess,
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
    pub const fn access(&self) -> Esp32C3RegionAccess {
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
pub struct Esp32C3PartitionTable {
    chip: &'static str,
    ota_slot_count: usize,
    regions: &'static [Esp32C3Region],
}

impl Esp32C3PartitionTable {
    /// Creates a build-validated projection of a complete ESP-IDF table.
    #[must_use]
    pub const fn new(
        chip: &'static str,
        ota_slot_count: usize,
        regions: &'static [Esp32C3Region],
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
    pub const fn regions(&self) -> &'static [Esp32C3Region] {
        self.regions
    }

    /// Finds a region by its native ESP-IDF label.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Esp32C3Region> {
        self.regions.iter().find(|region| region.name() == name)
    }
}

include!(concat!(env!("OUT_DIR"), "/esp32c3_layout.rs"));

#[cfg(target_arch = "riscv32")]
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

    use crate::{Esp32C3RegionAccess, BOARD_ESP32C3_PARTITION_TABLE};

    /// ESP HAL synchronous internal flash driver.
    pub type Esp32C3Flash<'d> = esp_storage::FlashStorage<'d>;

    /// One ESP-IDF partition backed by ESP HAL flash.
    pub type Esp32C3Partition =
        BlockingPartition<'static, CriticalSectionRawMutex, Esp32C3Flash<'static>>;

    /// Generic named ESP-IDF partitions available to System.
    pub type Esp32C3Partitions = Partitions<Esp32C3Partition, 16>;

    /// ESP Platform implementation for the selected ESP32-C3 target.
    pub struct Esp32C3Platform;

    /// Board/HAL bindings consumed by [`Esp32C3Platform`].
    pub struct Esp32C3PlatformBindings {
        ip_stack: Stack<'static>,
        tls: barracuda_tls::MbedTlsInput,
        flash: &'static Mutex<CriticalSectionRawMutex, RefCell<Esp32C3Flash<'static>>>,
    }

    impl Esp32C3PlatformBindings {
        /// Binds initialized ESP Platform services to a compatible Board.
        ///
        /// The IP runner must already be owned and running inside the ESP
        /// Platform network mechanism. The selected Target supplies the flash
        /// peripheral from its Board/HAL composition.
        ///
        /// # Errors
        ///
        /// Returns an error when the selected Board does not use ESP32-C3.
        pub fn from_initialized_services(
            board: &Board,
            ip_stack: Stack<'static>,
            tls: barracuda_tls::MbedTlsInput,
            flash: &'static Mutex<CriticalSectionRawMutex, RefCell<Esp32C3Flash<'static>>>,
        ) -> Result<Self, Esp32C3PlatformError> {
            if board.hardware().chip() != "esp32c3" {
                return Err(Esp32C3PlatformError::IncompatibleChip);
            }
            Ok(Self {
                ip_stack,
                tls,
                flash,
            })
        }
    }

    /// Turns the ESP HAL flash peripheral into the shared synchronous flash backend.
    #[must_use]
    pub fn flash<'d>(peripheral: esp_hal::peripherals::FLASH<'d>) -> Esp32C3Flash<'d> {
        esp_storage::FlashStorage::new(peripheral)
    }

    /// Projects every build-validated native entry from shared ESP flash.
    ///
    /// # Errors
    ///
    /// Returns an error if the Platform collection capacity is smaller than
    /// the selected ESP-IDF table.
    pub fn partitions(
        flash: &'static Mutex<CriticalSectionRawMutex, RefCell<Esp32C3Flash<'static>>>,
    ) -> Result<Esp32C3Partitions, barracuda_platform::PartitionsInsertError> {
        let mut partitions = Esp32C3Partitions::new();
        for region in BOARD_ESP32C3_PARTITION_TABLE.regions() {
            let access = match region.access() {
                Esp32C3RegionAccess::ReadOnly => PartitionAccess::ReadOnly,
                Esp32C3RegionAccess::ReadWrite => PartitionAccess::ReadWrite,
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

    impl Platform for Esp32C3Platform {
        type Bindings = Esp32C3PlatformBindings;
        type Tls = barracuda_tls::MbedTls;
        type Wifi = barracuda_platform::UnavailableWifiDevice;
        type Partitions = Esp32C3Partitions;
        type Error = Esp32C3PlatformError;

        fn prepare() -> Result<(), Self::Error> {
            barracuda_bulk_memory::platform::install_global();
            esp_println::logger::init_logger(crate::PLATFORM_LOG_LEVEL);
            log::info!("preparing ESP32-C3 Platform");
            Ok(())
        }

        async fn initialize(
            _spawner: Spawner,
            bindings: Self::Bindings,
        ) -> PlatformInitResult<Self> {
            log::info!("initializing ESP32-C3 Platform partitions");
            let partitions = partitions(bindings.flash)?;
            log::info!("initializing ESP32-C3 Platform TLS");
            let tls = bindings.tls.initialize()?;
            log::info!("initialized ESP32-C3 Platform");
            let ip_stack = bindings.ip_stack;
            Ok(PlatformResources {
                ip_stack,
                wifi: barracuda_platform::UnavailableWifiDevice::new(ip_stack),
                tls,
                partitions,
            })
        }
    }

    /// ESP32-C3 Platform initialization failure.
    #[derive(Debug)]
    pub enum Esp32C3PlatformError {
        /// The selected Board targets a different chip family.
        IncompatibleChip,
        /// The native table exceeded or violated the generic collection.
        Partitions(PartitionsInsertError),
        /// Platform TLS initialization failed.
        Tls(barracuda_tls::TlsError),
    }

    impl From<PartitionsInsertError> for Esp32C3PlatformError {
        fn from(error: PartitionsInsertError) -> Self {
            Self::Partitions(error)
        }
    }

    impl From<barracuda_tls::TlsError> for Esp32C3PlatformError {
        fn from(error: barracuda_tls::TlsError) -> Self {
            Self::Tls(error)
        }
    }

    impl core::fmt::Display for Esp32C3PlatformError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::IncompatibleChip => formatter.write_str("incompatible ESP32 chip"),
                Self::Partitions(error) => write!(formatter, "invalid ESP32 partitions: {error}"),
                Self::Tls(error) => write!(formatter, "failed to initialize ESP32 TLS: {error}"),
            }
        }
    }

    impl core::error::Error for Esp32C3PlatformError {}
}

#[cfg(target_arch = "riscv32")]
pub use internal_flash::{
    flash, partitions, Esp32C3Flash, Esp32C3Partition, Esp32C3Partitions, Esp32C3Platform,
    Esp32C3PlatformBindings, Esp32C3PlatformError,
};

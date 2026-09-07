//! STM32 Platform mechanisms backed by Board-native linker symbols.

#![no_std]

/// Runtime access discipline declared by the native linker memory region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stm32RegionAccess {
    /// The linker region does not carry the writable attribute.
    ReadOnly,
    /// The linker region carries the writable attribute.
    ReadWrite,
}

/// One flash-relative region resolved from Board-native linker symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkerRegion {
    offset: u32,
    size: u32,
}

impl LinkerRegion {
    /// Resolves absolute linker bounds against the HAL's flash base address.
    ///
    /// # Errors
    ///
    /// Returns an error when a symbol precedes flash, bounds are reversed, or
    /// the result cannot be represented by `embedded-storage` offsets.
    pub fn try_from_addresses(
        flash_base: usize,
        start: usize,
        end: usize,
    ) -> Result<Self, LinkerRegionError> {
        let offset = start
            .checked_sub(flash_base)
            .ok_or(LinkerRegionError::BeforeFlash)?;
        let size = end.checked_sub(start).ok_or(LinkerRegionError::Reversed)?;
        if size == 0 {
            return Err(LinkerRegionError::Empty);
        }
        Ok(Self {
            offset: u32::try_from(offset).map_err(|_error| LinkerRegionError::AddressWidth)?,
            size: u32::try_from(size).map_err(|_error| LinkerRegionError::AddressWidth)?,
        })
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
}

/// Invalid bounds exported by a Board-native linker script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkerRegionError {
    /// The start symbol is below the STM32 flash base.
    BeforeFlash,
    /// The end symbol is below the start symbol.
    Reversed,
    /// Start and end resolve to the same address.
    Empty,
    /// A flash-relative offset or size exceeds 32 bits.
    AddressWidth,
}

/// One named region from the Board-native linker layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stm32Region {
    name: &'static str,
    region: LinkerRegion,
    access: Stm32RegionAccess,
    filesystem: barracuda_platform::PartitionFilesystem,
}

impl Stm32Region {
    /// Creates a linker-resolved native region.
    #[must_use]
    pub const fn new(
        name: &'static str,
        region: LinkerRegion,
        access: Stm32RegionAccess,
        filesystem: barracuda_platform::PartitionFilesystem,
    ) -> Self {
        Self {
            name,
            region,
            access,
            filesystem,
        }
    }

    /// Returns the native linker-region name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the flash-relative bounds.
    #[must_use]
    pub const fn region(&self) -> &LinkerRegion {
        &self.region
    }

    /// Returns the native runtime access discipline.
    #[must_use]
    pub const fn access(&self) -> Stm32RegionAccess {
        self.access
    }

    /// Returns the filesystem declared by the linker partition table.
    #[must_use]
    pub const fn filesystem(&self) -> barracuda_platform::PartitionFilesystem {
        self.filesystem
    }
}

/// Complete set of named flash regions exported by the Board linker script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stm32PartitionTable<const N: usize> {
    regions: [Stm32Region; N],
}

impl<const N: usize> Stm32PartitionTable<N> {
    /// Creates a table after resolving all linker symbols.
    #[must_use]
    pub const fn new(regions: [Stm32Region; N]) -> Self {
        Self { regions }
    }

    /// Returns every linker-defined region in native declaration order.
    #[must_use]
    pub const fn regions(&self) -> &[Stm32Region] {
        &self.regions
    }

    /// Finds a region by its native linker symbol name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Stm32Region> {
        self.regions.iter().find(|region| region.name() == name)
    }
}

include!(concat!(env!("OUT_DIR"), "/stm32_layout.rs"));

#[cfg(all(feature = "stm32f429zi", target_arch = "arm"))]
mod internal_flash {
    use core::cell::RefCell;

    use barracuda_board::Board;
    use barracuda_platform::{
        NamedPartition, PartitionAccess, Partitions, Platform, PlatformInitResult,
        PlatformResources,
    };
    use embassy_embedded_hal::flash::partition::BlockingPartition;
    use embassy_executor::Spawner;
    use embassy_net::Stack;
    use embassy_stm32::flash::{Async, Flash};
    use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};

    use crate::{board_partition_table, LinkerRegionError, Stm32RegionAccess};

    /// STM32 internal flash driver with both blocking and asynchronous HAL APIs.
    pub type Stm32Flash = Flash<'static, Async>;

    /// One linker-defined partition backed by Embassy STM32 flash.
    pub type Stm32Partition = BlockingPartition<'static, CriticalSectionRawMutex, Stm32Flash>;

    /// Generic named STM32 partitions available to System.
    pub type Stm32Partitions = Partitions<Stm32Partition, 16>;

    /// STM32 Platform implementation for the selected STM32F429 target.
    pub struct Stm32Platform;

    /// Board/HAL bindings consumed by [`Stm32Platform`].
    pub struct Stm32PlatformBindings {
        ip_stack: Stack<'static>,
        tls: barracuda_tls::MbedTlsInput,
        flash: &'static Mutex<CriticalSectionRawMutex, RefCell<Stm32Flash>>,
    }

    impl Stm32PlatformBindings {
        /// Binds initialized STM32 Platform services to a compatible Board.
        ///
        /// The selected Board/HAL composition owns the Ethernet wiring and
        /// hands the resulting Platform IP service and internal flash to this
        /// binding.
        ///
        /// # Errors
        ///
        /// Returns an error when the selected Board does not use STM32F429ZI.
        pub fn from_initialized_services(
            board: &Board,
            ip_stack: Stack<'static>,
            tls: barracuda_tls::MbedTlsInput,
            flash: &'static Mutex<CriticalSectionRawMutex, RefCell<Stm32Flash>>,
        ) -> Result<Self, Stm32PlatformError> {
            if board.hardware().chip() != "stm32f429zi" {
                return Err(Stm32PlatformError::IncompatibleChip);
            }
            Ok(Self {
                ip_stack,
                tls,
                flash,
            })
        }
    }

    /// Projects every Board-native linker region from shared STM32 flash.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid linker bounds or insufficient collection
    /// capacity.
    pub fn partitions(
        flash: &'static Mutex<CriticalSectionRawMutex, RefCell<Stm32Flash>>,
    ) -> Result<Stm32Partitions, Stm32PartitionsError> {
        let table = board_partition_table(embassy_stm32::flash::FLASH_BASE)?;
        let mut partitions = Stm32Partitions::new();
        for region in table.regions() {
            let access = match region.access() {
                Stm32RegionAccess::ReadOnly => PartitionAccess::ReadOnly,
                Stm32RegionAccess::ReadWrite => PartitionAccess::ReadWrite,
            };
            let bounds = region.region();
            partitions.insert(NamedPartition::new(
                region.name(),
                access,
                region.filesystem(),
                BlockingPartition::new(flash, bounds.offset(), bounds.size()),
            ))?;
        }
        Ok(partitions)
    }

    /// Failure while projecting STM32 native partitions.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Stm32PartitionsError {
        /// Board linker symbols do not form valid flash-relative bounds.
        Linker(LinkerRegionError),
        /// The generic collection capacity is smaller than the linker layout.
        Capacity(barracuda_platform::PartitionsInsertError),
    }

    impl From<LinkerRegionError> for Stm32PartitionsError {
        fn from(error: LinkerRegionError) -> Self {
            Self::Linker(error)
        }
    }

    impl From<barracuda_platform::PartitionsInsertError> for Stm32PartitionsError {
        fn from(error: barracuda_platform::PartitionsInsertError) -> Self {
            Self::Capacity(error)
        }
    }

    impl Platform for Stm32Platform {
        type Bindings = Stm32PlatformBindings;
        type Tls = barracuda_tls::MbedTls;
        type Partitions = Stm32Partitions;
        type Error = Stm32PlatformError;

        fn prepare() -> Result<(), Self::Error> {
            rtt_target::rtt_init_log!(crate::PLATFORM_LOG_LEVEL);
            log::info!("preparing STM32 Platform");
            Ok(())
        }

        async fn initialize(
            _spawner: Spawner,
            bindings: Self::Bindings,
        ) -> PlatformInitResult<Self> {
            log::info!("initializing STM32 Platform partitions");
            let partitions = partitions(bindings.flash)?;
            log::info!("initializing STM32 Platform TLS");
            let tls = bindings.tls.initialize()?;
            log::info!("initialized STM32 Platform");
            Ok(PlatformResources {
                ip_stack: bindings.ip_stack,
                tls,
                partitions,
            })
        }
    }

    /// STM32F429 Platform initialization failure.
    #[derive(Debug)]
    pub enum Stm32PlatformError {
        /// The selected Board targets a different chip family.
        IncompatibleChip,
        /// Native linker regions could not become generic partitions.
        Partitions(Stm32PartitionsError),
        /// Platform TLS initialization failed.
        Tls(barracuda_tls::TlsError),
    }

    impl From<Stm32PartitionsError> for Stm32PlatformError {
        fn from(error: Stm32PartitionsError) -> Self {
            Self::Partitions(error)
        }
    }

    impl From<barracuda_tls::TlsError> for Stm32PlatformError {
        fn from(error: barracuda_tls::TlsError) -> Self {
            Self::Tls(error)
        }
    }

    impl core::fmt::Display for Stm32PlatformError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::IncompatibleChip => formatter.write_str("incompatible STM32 chip"),
                Self::Partitions(error) => {
                    write!(formatter, "invalid STM32 partitions: {error:?}")
                }
                Self::Tls(error) => write!(formatter, "failed to initialize STM32 TLS: {error}"),
            }
        }
    }

    impl core::error::Error for Stm32PlatformError {}
}

#[cfg(all(feature = "stm32f429zi", target_arch = "arm"))]
pub use internal_flash::{
    partitions, Stm32Flash, Stm32Partition, Stm32Partitions, Stm32PartitionsError, Stm32Platform,
    Stm32PlatformBindings, Stm32PlatformError,
};

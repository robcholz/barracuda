//! STM32-native Board storage integration.
//!
//! This crate proves the flash/layout portion of the STM32 Platform. It does
//! not publish a selectable `platform.yml` until network and filesystem
//! initialization complete the `barracuda_platform::Platform` contract.

#![no_std]

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

/// Storage regions resolved from the selected Board's native linker script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stm32StorageLayout {
    filesystem: LinkerRegion,
    web_assets: Option<LinkerRegion>,
    database: LinkerRegion,
}

impl Stm32StorageLayout {
    /// Creates a layout after resolving its linker symbols.
    #[must_use]
    pub const fn new(
        filesystem: LinkerRegion,
        web_assets: Option<LinkerRegion>,
        database: LinkerRegion,
    ) -> Self {
        Self {
            filesystem,
            web_assets,
            database,
        }
    }

    /// Returns the writable filesystem region.
    #[must_use]
    pub const fn filesystem(&self) -> &LinkerRegion {
        &self.filesystem
    }

    /// Returns the optional provisioned Web asset region.
    #[must_use]
    pub const fn web_assets(&self) -> Option<&LinkerRegion> {
        self.web_assets.as_ref()
    }

    /// Returns the writable Ekv region.
    #[must_use]
    pub const fn database(&self) -> &LinkerRegion {
        &self.database
    }
}

include!(concat!(env!("OUT_DIR"), "/stm32_layout.rs"));

#[cfg(all(feature = "stm32f429zi", target_arch = "arm"))]
mod hal {
    use embassy_embedded_hal::flash::partition::Partition;
    use embassy_stm32::flash::{Async, Flash};
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};

    use crate::{board_storage_layout, LinkerRegionError};

    /// STM32 internal flash using Embassy's asynchronous HAL driver.
    pub type Stm32AsyncFlash = Flash<'static, Async>;

    /// Ekv partition backed by Embassy STM32 flash.
    pub type Stm32DatabaseRegion = Partition<'static, CriticalSectionRawMutex, Stm32AsyncFlash>;

    /// Projects the native linker-defined database region from shared STM32 flash.
    ///
    /// # Errors
    ///
    /// Returns an error when the Board linker symbols do not form valid flash bounds.
    pub fn database_region(
        flash: &'static Mutex<CriticalSectionRawMutex, Stm32AsyncFlash>,
    ) -> Result<Stm32DatabaseRegion, LinkerRegionError> {
        let layout = board_storage_layout(embassy_stm32::flash::FLASH_BASE)?;
        let region = layout.database();
        Ok(Partition::new(flash, region.offset(), region.size()))
    }
}

#[cfg(all(feature = "stm32f429zi", target_arch = "arm"))]
pub use hal::{database_region, Stm32AsyncFlash, Stm32DatabaseRegion};

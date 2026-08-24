//! ESP32-native Board storage integration.
//!
//! This crate proves the flash/layout portion of the ESP32 Platform. It does
//! not publish a selectable `platform.yml` until network and filesystem
//! initialization complete the `barracuda_platform::Platform` contract.

#![no_std]

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
}

impl Esp32Region {
    /// Creates a generated native ESP32 region.
    #[must_use]
    pub const fn new(
        name: &'static str,
        offset: u32,
        size: u32,
        access: Esp32RegionAccess,
    ) -> Self {
        Self {
            name,
            offset,
            size,
            access,
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
}

/// Storage capabilities projected from one native ESP-IDF table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Esp32StorageLayout {
    chip: &'static str,
    ota_slot_count: usize,
    database: Esp32Region,
    filesystem: Esp32Region,
    web_assets: Option<Esp32Region>,
}

impl Esp32StorageLayout {
    /// Creates a build-validated ESP32 storage projection.
    #[must_use]
    pub const fn new(
        chip: &'static str,
        ota_slot_count: usize,
        database: Esp32Region,
        filesystem: Esp32Region,
        web_assets: Option<Esp32Region>,
    ) -> Self {
        Self {
            chip,
            ota_slot_count,
            database,
            filesystem,
            web_assets,
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

    /// Returns the writable Ekv region.
    #[must_use]
    pub const fn database(&self) -> &Esp32Region {
        &self.database
    }

    /// Returns the writable filesystem region.
    #[must_use]
    pub const fn filesystem(&self) -> &Esp32Region {
        &self.filesystem
    }

    /// Returns the optional provisioned Web asset region.
    #[must_use]
    pub const fn web_assets(&self) -> Option<&Esp32Region> {
        self.web_assets.as_ref()
    }
}

include!(concat!(env!("OUT_DIR"), "/esp32_layout.rs"));

#[cfg(all(feature = "esp32c6", target_arch = "riscv32"))]
mod hal {
    use embassy_embedded_hal::{adapter::BlockingAsync, flash::partition::Partition};
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};

    use crate::BOARD_ESP32_LAYOUT;

    /// ESP HAL flash adapted to Embassy's asynchronous NOR contract.
    pub type Esp32AsyncFlash<'d> = BlockingAsync<esp_storage::FlashStorage<'d>>;

    /// Ekv partition backed by ESP HAL flash.
    pub type Esp32DatabaseRegion =
        Partition<'static, CriticalSectionRawMutex, Esp32AsyncFlash<'static>>;

    /// Turns the ESP HAL flash peripheral into the shared asynchronous flash backend.
    #[must_use]
    pub fn async_flash<'d>(flash: esp_hal::peripherals::FLASH<'d>) -> Esp32AsyncFlash<'d> {
        BlockingAsync::new(esp_storage::FlashStorage::new(flash))
    }

    /// Projects the build-validated native database partition from shared ESP flash.
    #[must_use]
    pub fn database_region(
        flash: &'static Mutex<CriticalSectionRawMutex, Esp32AsyncFlash<'static>>,
    ) -> Esp32DatabaseRegion {
        let region = BOARD_ESP32_LAYOUT.database();
        Partition::new(flash, region.offset(), region.size())
    }
}

#[cfg(all(feature = "esp32c6", target_arch = "riscv32"))]
pub use hal::{async_flash, database_region, Esp32AsyncFlash, Esp32DatabaseRegion};

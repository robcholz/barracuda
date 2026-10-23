//! ESP32 process entry and bootstrap resources.

use crate::{
    Esp32Flash as Flash, Esp32PlatformBindings as PlatformBindings,
    Esp32PlatformError as PlatformError, Esp32Wifi as Wifi,
};
#[cfg(feature = "wifi")]
use crate::{Esp32WifiDevice as WifiDevice, Esp32WifiError as WifiError};

/// The chip named in bootstrap failures.
#[doc(hidden)]
pub const CHIP: &str = "ESP32";

/// Global heap bytes in the main DRAM segment, beside the static data.
const HEAP_BYTES: usize = 16 * 1024;
/// Global heap bytes in the DRAM the bootloader frees, about 96 KiB here.
const RECLAIMED_HEAP_BYTES: usize = 96 * 1024;

include!("application/radio.rs");

static EXTERNAL_MEMORY_ALLOCATOR: esp_alloc::ExternalMemory = esp_alloc::ExternalMemory;
static BULK_MEMORY_BACKEND: barracuda_bulk_memory::platform::Backend =
    barracuda_bulk_memory::platform::Backend::new(&EXTERNAL_MEMORY_ALLOCATOR);

/// The most external memory the ESP32 cache can map.
const MAPPABLE_EXTERNAL_MEMORY_BYTES: usize = 4 * 1024 * 1024;

/// Initializes Board-declared external memory as the explicit-use bulk domain.
///
/// The ESP32 maps at most 4 MiB of quad-SPI PSRAM; a larger part maps its
/// first 4 MiB. Without external memory, bulk memory is the global heap.
#[doc(hidden)]
pub fn initialize_bulk_memory(
    board: &barracuda_board::Board,
    psram: esp_hal::peripherals::PSRAM<'static>,
) {
    barracuda_bulk_memory::platform::install_global();
    let Some(memory) = board.hardware().external_memory() else {
        return;
    };
    if memory.interface() != barracuda_board::ExternalMemoryInterface::QuadSpi {
        esp_println::println!("ESP32 supports only quad-SPI PSRAM; external memory stays unused");
        return;
    }
    let config = esp_hal::psram::PsramConfig {
        size: esp_hal::psram::PsramSize::Size(
            memory.size_bytes().min(MAPPABLE_EXTERNAL_MEMORY_BYTES),
        ),
        ..Default::default()
    };
    esp_alloc::psram_allocator!(psram, esp_hal::psram, config);
    barracuda_bulk_memory::platform::install(&BULK_MEMORY_BACKEND);
}

/// Initializes bulk memory inside the generated entry.
#[doc(hidden)]
#[macro_export]
macro_rules! __initialize_bulk_memory {
    ($board:expr, $peripherals:ident) => {
        $crate::application::initialize_bulk_memory($board, $peripherals.PSRAM)
    };
}

//! ESP32-S3 process entry and bootstrap resources.

use crate::{
    Esp32S3Flash as Flash, Esp32S3PlatformBindings as PlatformBindings,
    Esp32S3PlatformError as PlatformError, Esp32S3WifiDevice as WifiDevice,
    Esp32S3WifiError as WifiError,
};

/// The chip named in bootstrap failures.
#[doc(hidden)]
pub const CHIP: &str = "ESP32-S3";

include!("../../esp32/src/application/radio.rs");

static EXTERNAL_MEMORY_ALLOCATOR: esp_alloc::ExternalMemory = esp_alloc::ExternalMemory;
static BULK_MEMORY_BACKEND: barracuda_bulk_memory::platform::Backend =
    barracuda_bulk_memory::platform::Backend::new(&EXTERNAL_MEMORY_ALLOCATOR);

/// Initializes Board-declared external memory as the explicit-use bulk domain.
#[doc(hidden)]
pub fn initialize_bulk_memory(
    board: &barracuda_board::Board,
    psram: esp_hal::peripherals::PSRAM<'static>,
) {
    barracuda_bulk_memory::platform::install_global();
    let Some(memory) = board.hardware().external_memory() else {
        return;
    };
    let mode = match memory.interface() {
        barracuda_board::ExternalMemoryInterface::QuadSpi => esp_hal::psram::PsramMode::QuadSpi,
        barracuda_board::ExternalMemoryInterface::OctalSpi => esp_hal::psram::PsramMode::OctalSpi,
    };
    let config = esp_hal::psram::PsramConfig {
        mode,
        size: esp_hal::psram::PsramSize::Size(memory.size_bytes()),
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

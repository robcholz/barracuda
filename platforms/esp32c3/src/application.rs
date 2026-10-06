//! ESP32-C3 process entry and bootstrap resources.

use crate::{
    Esp32C3Flash as Flash, Esp32C3PlatformBindings as PlatformBindings,
    Esp32C3PlatformError as PlatformError, Esp32C3WifiDevice as WifiDevice,
    Esp32C3WifiError as WifiError,
};

/// The chip named in bootstrap failures.
#[doc(hidden)]
pub const CHIP: &str = "ESP32-C3";

include!("../../esp32/src/application/radio.rs");

/// Serves bulk memory from the global heap.
///
/// No ESP32-C3 Board declares external memory, so the Platform has no
/// separate bulk domain.
#[doc(hidden)]
pub fn initialize_bulk_memory(_board: &barracuda_board::Board) {
    barracuda_bulk_memory::platform::install_global();
}

/// Initializes bulk memory inside the generated entry.
#[doc(hidden)]
#[macro_export]
macro_rules! __initialize_bulk_memory {
    ($board:expr, $peripherals:ident) => {
        $crate::application::initialize_bulk_memory($board)
    };
}

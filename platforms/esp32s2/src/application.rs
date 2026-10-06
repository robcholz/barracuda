//! ESP32-S2 process entry and bootstrap resources.

use crate::{
    Esp32S2Flash as Flash, Esp32S2PlatformBindings as PlatformBindings,
    Esp32S2PlatformError as PlatformError, Esp32S2WifiDevice as WifiDevice,
    Esp32S2WifiError as WifiError,
};

/// The chip named in bootstrap failures.
#[doc(hidden)]
pub const CHIP: &str = "ESP32-S2";

include!("../../esp32/src/application/radio.rs");

/// Serves bulk memory from the global heap.
///
/// No ESP32-S2 Board declares external memory, so the Platform has no
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

//! ESP32-P4 implementation of Barracuda's stable HAL surface.

mod dsi;

pub use dsi::{
    dsi_backlight_none, dsi_backlight_pin, dsi_host, DsiBacklight, DsiConfigError, DsiError,
    DsiHost, ScanoutChannel,
};

include!("../../esp32/src/hal.rs");

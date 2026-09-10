//! ESP32-P4 implementation of Barracuda's stable HAL surface.

include!("../../esp32/src/hal.rs");

include!("../../esp32/src/i2s.rs");

mod dsi;
mod sdmmc;

pub use dsi::{dsi_host, DsiConfigError, DsiHost};
pub use sdmmc::{sdmmc_device, SdmmcConfigError, SdmmcDevice};

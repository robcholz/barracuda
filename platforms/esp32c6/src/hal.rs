//! ESP32-C6 implementation of Barracuda's stable HAL surface.

// esp-hal's PCNT driver is the same on the ESP32, ESP32-S3, and ESP32-C6.
#[path = "../../esp32s3/src/hal/pulse_counter.rs"]
mod pulse_counter;

pub use pulse_counter::{quadrature_counter, QuadratureCounter};

include!("../../esp32/src/hal.rs");

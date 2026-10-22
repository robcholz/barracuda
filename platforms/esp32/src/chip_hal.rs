//! ESP32 adaptation from Board-selected tokens to hardware resources.
//!
//! The vendor adaptation every ESP Platform shares is `hal.rs`; this module
//! adds the ESP32's media bindings, whose adapters it shares with the
//! ESP32-S3.

// esp-hal drives the ESP32's I2S and SD host with the same drivers as the
// ESP32-S3's, so the two Platforms share the adapters.
#[path = "../../esp32s3/src/hal/i2s_stream.rs"]
mod i2s_stream;
#[path = "../../esp32s3/src/hal/pdm_stream.rs"]
mod pdm_stream;
// esp-hal's PCNT driver is the same on the ESP32, ESP32-S3, and ESP32-C6.
#[path = "../../esp32s3/src/hal/pulse_counter.rs"]
mod pulse_counter;
#[path = "../../esp32s3/src/hal/sdmmc.rs"]
mod sdmmc;

pub use i2s_stream::{i2s_stream, I2sConfigError, I2sDevice, I2sTransferError};
pub use pdm_stream::{pdm_i2s_stream, PdmDmaChannel, PdmI2sStream};
pub use pulse_counter::{quadrature_counter, QuadratureCounter};
pub use sdmmc::{sdmmc_device_1bit, SdmmcConfigError, SdmmcDevice};

/// The SD host slot a Board's microSD card uses. Slot 0 is the eight-bit
/// slot on the pins of the module's flash; slot 1 drives the fixed IO_MUX
/// pins CLK GPIO14, CMD GPIO15, and D0 GPIO2.
const SDMMC_SLOT: u8 = 1;

pdm_stream::pdm_dma_channels!(DMA_I2S0);

include!("hal.rs");

/// Statically allocates independent transmit and receive I2S DMA buffers.
#[macro_export]
macro_rules! __barracuda_esp32_i2s_dma_buffers {
    ($bytes:expr) => {{
        match (
            $crate::hal::__vendor::dma_tx_buffer!($bytes),
            $crate::hal::__vendor::dma_rx_buffer!($bytes),
        ) {
            (Ok(tx), Ok(rx)) => Ok((tx, rx)),
            (Err(error), _) | (_, Err(error)) => Err($crate::hal::I2sConfigError::DmaBuffer(error)),
        }
    }};
}

/// Resolves a Board's SD host slot binding to its token type: the Board
/// composition takes the SD host itself and builds the card's slot 1 from it.
#[macro_export]
macro_rules! __barracuda_esp32_sdmmc_slot_binding_type {
    ($controller:ident, 1) => {
        $crate::hal::__vendor::peripherals::$controller<'static>
    };
}

/// Moves a Board's SD host slot binding out of the chip peripherals; the
/// ESP32 entry hands the Board composition nothing else.
#[macro_export]
macro_rules! __barracuda_esp32_sdmmc_slot_binding {
    ($peripherals:ident, $handoff:expr, $controller:ident, 1) => {
        $peripherals.$controller
    };
}

#[doc(hidden)]
pub use __barracuda_esp32_i2s_dma_buffers as i2s_dma_buffers;
#[doc(hidden)]
pub use __barracuda_esp32_sdmmc_slot_binding as sdmmc_slot_binding;
#[doc(hidden)]
pub use __barracuda_esp32_sdmmc_slot_binding_type as sdmmc_slot_binding_type;

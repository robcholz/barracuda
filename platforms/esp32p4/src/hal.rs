//! ESP32-P4 implementation of Barracuda's stable HAL surface.

mod dsi;
mod sdmmc;
// esp-hal drives the ESP32-P4's I2S controllers with the same master driver
// as the ESP32-S3's, so the two Platforms share the stream adapter.
#[path = "../../esp32s3/src/hal/i2s_stream.rs"]
mod i2s_stream;

pub use dsi::{
    dsi_backlight_none, dsi_backlight_pin, dsi_host, DsiBacklight, DsiConfigError, DsiError,
    DsiHost, ScanoutChannel,
};
pub use i2s_stream::{
    i2s_stream, i2s_stream_with_mclk, I2sConfigError, I2sDevice, I2sTransferError,
};
pub use sdmmc::{
    sdmmc_device, sdmmc_device_1bit, sdmmc_slot_binding, sdmmc_slot_binding_type, CardBus,
    SdCardSlot, SdmmcConfigError, SdmmcDevice, SdmmcError,
};

include!("../../esp32/src/hal.rs");

/// Statically allocates independent transmit and receive I2S DMA buffers.
#[macro_export]
macro_rules! __barracuda_esp32p4_i2s_dma_buffers {
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

#[doc(hidden)]
pub use __barracuda_esp32p4_i2s_dma_buffers as i2s_dma_buffers;

//! ESP32-S3 adaptation from Board-selected tokens to hardware resources.

mod camera;
mod capacitive_touch;
mod i2s_stream;
mod parallel_output;
mod pdm_stream;
mod pulse_counter;
mod sdmmc;

pub use camera::{
    camera_capture, camera_capture_without_master_clock, camera_dma_buffer_from_bulk_memory,
    CameraCaptureError, CameraConfigError, CameraReceiver, CAMERA_DMA_ALIGNMENT,
};
pub use capacitive_touch::{capacitive_touch_buttons, CapacitiveTouchButtons};
pub use i2s_stream::{
    i2s_stream, i2s_stream_with_mclk, I2sConfigError, I2sDevice, I2sTransferError,
};
pub use pdm_stream::{pdm_i2s_stream, PdmDmaChannel, PdmI2sStream};
pub use pulse_counter::{quadrature_counter, QuadratureCounter};

/// The SD host slot a Board's microSD card uses; both slots reach any pin
/// through the GPIO matrix.
const SDMMC_SLOT: u8 = 0;

pdm_stream::pdm_dma_channels!(DMA_CH0, DMA_CH1, DMA_CH2, DMA_CH3, DMA_CH4);
pub use parallel_output::{
    parallel_output, parallel_output_data_only, ParallelOutputBus, ParallelOutputConfigError,
    ParallelOutputError,
};
pub use sdmmc::{sdmmc_device_1bit, SdmmcConfigError, SdmmcDevice};

include!("../../esp32/src/hal.rs");

use barracuda_board_hal::QuadSpiBus;
use esp_hal::{
    dma::{DmaRxBuf, DmaTxBuf},
    gpio::NoPin,
    spi::master::{Address as SpiAddress, Command as SpiCommand, DataMode},
};

/// ESP32-S3 four-data-line half-duplex SPI construction failure.
pub type QuadSpiConfigError = SpiConfigError;

/// ESP32-S3 selected QSPI device.
pub struct QuadSpiDevice {
    spi: Spi<'static, Blocking>,
}

impl QuadSpiBus for QuadSpiDevice {
    type Error = esp_hal::spi::Error;

    fn write(&mut self, opcode: u8, address: u32, data: &[u8]) -> Result<(), Self::Error> {
        self.spi.half_duplex_write(
            DataMode::Quad,
            SpiCommand::_8Bit(u16::from(opcode), DataMode::Single),
            SpiAddress::_24Bit(address, DataMode::Single),
            0,
            data,
        )
    }

    fn write_single_line(
        &mut self,
        opcode: u8,
        address: u32,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        self.spi.half_duplex_write(
            DataMode::Single,
            SpiCommand::_8Bit(u16::from(opcode), DataMode::Single),
            SpiAddress::_24Bit(address, DataMode::Single),
            0,
            data,
        )
    }
}

/// Constructs one ESP32-S3 QSPI device from Board-selected resources.
///
/// # Errors
///
/// Returns [`QuadSpiConfigError`] when the requested clock cannot be configured.
#[allow(clippy::too_many_arguments)]
pub fn quad_spi_device(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    data0: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    data1: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    data2: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    data3: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    chip_select: impl PeripheralOutput<'static> + 'static,
    frequency_hz: u32,
) -> Result<QuadSpiDevice, QuadSpiConfigError> {
    let spi = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_sio0(data0)
    .with_sio1(data1)
    .with_sio2(data2)
    .with_sio3(data3)
    .with_cs(chip_select);
    Ok(QuadSpiDevice { spi })
}

/// Statically allocates independent transmit and receive I2S DMA buffers.
#[macro_export]
macro_rules! __barracuda_esp32s3_i2s_dma_buffers {
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

/// Statically allocates one parallel-output DMA staging buffer at the
/// generated Board call site.
#[macro_export]
macro_rules! __barracuda_esp32s3_parallel_output_dma_buffer {
    ($bytes:expr) => {
        $crate::hal::__vendor::dma_tx_buffer!($bytes)
            .map_err($crate::hal::ParallelOutputConfigError::DmaBuffer)
    };
}

/// Resolves a Board's SD host slot binding to its token type: on the
/// ESP32-S3 the Board composition takes the SD host itself and builds the
/// card's slot 0 from it, so the binding is the `SDHOST` token.
#[macro_export]
macro_rules! __barracuda_esp32s3_sdmmc_slot_binding_type {
    ($controller:ident, 0) => {
        $crate::hal::__vendor::peripherals::$controller<'static>
    };
}

/// Moves a Board's SD host slot binding out of the chip peripherals; the
/// ESP32-S3 entry hands the Board composition nothing else.
#[macro_export]
macro_rules! __barracuda_esp32s3_sdmmc_slot_binding {
    ($peripherals:ident, $handoff:expr, $controller:ident, 0) => {
        $peripherals.$controller
    };
}

#[doc(hidden)]
pub use crate::__barracuda_esp32s3_camera_dma_buffer as camera_dma_buffer;
#[doc(hidden)]
pub use __barracuda_esp32s3_i2s_dma_buffers as i2s_dma_buffers;
#[doc(hidden)]
pub use __barracuda_esp32s3_i2s_dma_buffers as runtime_i2s_dma_buffers;
#[doc(hidden)]
pub use __barracuda_esp32s3_parallel_output_dma_buffer as parallel_output_dma_buffer;
#[doc(hidden)]
pub use __barracuda_esp32s3_sdmmc_slot_binding as sdmmc_slot_binding;
#[doc(hidden)]
pub use __barracuda_esp32s3_sdmmc_slot_binding_type as sdmmc_slot_binding_type;

/// Error surfaced while generated runtime I2S DMA storage is created.
pub type RuntimeI2sResourceError = I2sConfigError;

/// Platform-owned ESP32-S3 I2S controller, DMA channel, and bounded buffers.
pub struct RuntimeI2sResource {
    controller: esp_hal::peripherals::I2S0<'static>,
    dma: Option<esp_hal::peripherals::DMA_CH0<'static>>,
    tx_buffer: DmaTxBuf,
    rx_buffer: DmaRxBuf,
}

impl barracuda_board_hal::RuntimeI2sPlatform for RuntimeAdapter {
    type I2s = I2sDevice;
    type I2sError = I2sConfigError;

    fn supports_i2s(
        resource: &Self::I2sResource,
        _bclk: &Self::PinToken,
        _ws: &Self::PinToken,
        dout: Option<&Self::PinToken>,
        din: Option<&Self::PinToken>,
        _mclk: Option<&Self::PinToken>,
    ) -> bool {
        resource.dma.is_some() && (dout.is_some() || din.is_some())
    }

    fn supports_i2s_format(
        resource: &Self::I2sResource,
        bclk: &Self::PinToken,
        ws: &Self::PinToken,
        dout: Option<&Self::PinToken>,
        din: Option<&Self::PinToken>,
        mclk: Option<&Self::PinToken>,
        format: barracuda_board_hal::audio::PcmFormat,
    ) -> bool {
        let valid_master_clock = match (mclk, format.master_clock_hz) {
            (None, None) => true,
            (Some(_), Some(clock)) => format
                .sample_rate_hz
                .checked_mul(256)
                .is_some_and(|expected| expected == clock),
            _ => false,
        };
        format.sample_rate_hz > 0
            && matches!(format.channels, 1 | 2)
            && format.bits_per_sample == 16
            && valid_master_clock
            && Self::supports_i2s(resource, bclk, ws, dout, din, mclk)
    }

    fn i2s(
        mut resource: Self::I2sResource,
        bclk: Self::PinToken,
        ws: Self::PinToken,
        dout: Option<Self::PinToken>,
        din: Option<Self::PinToken>,
        mclk: Option<Self::PinToken>,
        format: barracuda_board_hal::audio::PcmFormat,
    ) -> Result<Self::I2s, Self::I2sError> {
        let dma = resource.dma.take().ok_or(I2sConfigError::MissingDma)?;
        match (dout, din, mclk) {
            (Some(dout), Some(din), Some(mclk)) => i2s_stream_with_mclk(
                resource.controller,
                dma,
                bclk,
                ws,
                dout,
                din,
                mclk,
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
                resource.tx_buffer,
                resource.rx_buffer,
            ),
            (Some(dout), Some(din), None) => i2s_stream(
                resource.controller,
                dma,
                bclk,
                ws,
                dout,
                din,
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
                resource.tx_buffer,
                resource.rx_buffer,
            ),
            (Some(dout), None, Some(mclk)) => i2s_stream_with_mclk(
                resource.controller,
                dma,
                bclk,
                ws,
                dout,
                NoPin,
                mclk,
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
                resource.tx_buffer,
                resource.rx_buffer,
            ),
            (Some(dout), None, None) => i2s_stream(
                resource.controller,
                dma,
                bclk,
                ws,
                dout,
                NoPin,
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
                resource.tx_buffer,
                resource.rx_buffer,
            ),
            (None, Some(din), Some(mclk)) => i2s_stream_with_mclk(
                resource.controller,
                dma,
                bclk,
                ws,
                NoPin,
                din,
                mclk,
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
                resource.tx_buffer,
                resource.rx_buffer,
            ),
            (None, Some(din), None) => i2s_stream(
                resource.controller,
                dma,
                bclk,
                ws,
                NoPin,
                din,
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
                resource.tx_buffer,
                resource.rx_buffer,
            ),
            (None, None, _) => Err(I2sConfigError::UnsupportedFormat),
        }
    }
}

/// Preserves ownership of one generated ESP32-S3 I2S DMA channel token.
#[must_use]
pub fn runtime_i2s_dma(
    dma: esp_hal::peripherals::DMA_CH0<'static>,
) -> esp_hal::peripherals::DMA_CH0<'static> {
    dma
}

/// Builds one runtime I2S allocation from a controller, DMA pool, and buffers.
#[must_use]
pub fn runtime_i2s_resource<const N: usize>(
    controller: esp_hal::peripherals::I2S0<'static>,
    dma: [esp_hal::peripherals::DMA_CH0<'static>; N],
    buffers: (DmaTxBuf, DmaRxBuf),
) -> RuntimeI2sResource {
    let (tx_buffer, rx_buffer) = buffers;
    RuntimeI2sResource {
        controller,
        dma: dma.into_iter().next(),
        tx_buffer,
        rx_buffer,
    }
}

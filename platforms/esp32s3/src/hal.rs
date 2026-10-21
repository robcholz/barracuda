//! ESP32-S3 adaptation from Board-selected tokens to hardware resources.

mod capacitive_touch;
mod i2s_stream;
mod parallel_output;
mod sdmmc;

pub use capacitive_touch::{capacitive_touch_buttons, CapacitiveTouchButtons};
pub use i2s_stream::{
    i2s_stream, i2s_stream_with_mclk, I2sConfigError, I2sDevice, I2sTransferError,
};
pub use parallel_output::{
    parallel_output, parallel_output_data_only, ParallelOutputBus, ParallelOutputConfigError,
    ParallelOutputError,
};
pub use sdmmc::{sdmmc_device_1bit, SdmmcConfigError, SdmmcDevice};

include!("../../esp32/src/hal.rs");

use barracuda_board_hal::QuadSpiBus;
use barracuda_peripheral::camera::{FrameReceiver, FrameReceiverErrorType};
use esp_hal::{
    dma::{DmaBufError, DmaError, DmaRxBuf, DmaTxBuf},
    gpio::NoPin,
    lcd_cam::{
        cam::{
            Camera as VendorCamera, Config as VendorCameraConfig,
            ConfigError as VendorCameraConfigError,
        },
        CamDmaRxChannel, LcdCam,
    },
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

/// Statically allocates one ESP32-S3 DVP DMA buffer at the generated Board call site.
#[macro_export]
macro_rules! __barracuda_esp32s3_camera_dma_buffer {
    ($bytes:expr) => {
        $crate::hal::__vendor::dma_rx_buffer!($bytes)
            .map_err($crate::hal::CameraConfigError::DmaBuffer)
    };
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

#[doc(hidden)]
pub use __barracuda_esp32s3_camera_dma_buffer as camera_dma_buffer;
#[doc(hidden)]
pub use __barracuda_esp32s3_i2s_dma_buffers as i2s_dma_buffers;
#[doc(hidden)]
pub use __barracuda_esp32s3_i2s_dma_buffers as runtime_i2s_dma_buffers;
#[doc(hidden)]
pub use __barracuda_esp32s3_parallel_output_dma_buffer as parallel_output_dma_buffer;

/// ESP32-S3 camera receiver construction failure.
#[derive(Debug)]
pub enum CameraConfigError {
    /// The Board-selected DMA storage cannot be used by the controller.
    DmaBuffer(DmaBufError),
    /// The requested camera clock or signal configuration is unsupported.
    Peripheral(VendorCameraConfigError),
}

impl core::fmt::Display for CameraConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DmaBuffer(_) => formatter.write_str("invalid camera DMA buffer"),
            Self::Peripheral(_) => formatter.write_str("invalid ESP32-S3 camera configuration"),
        }
    }
}

impl core::error::Error for CameraConfigError {}

/// ESP32-S3 frame-capture failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraCaptureError {
    /// A capture is already in progress or the receiver lost ownership state.
    Busy,
    /// The caller supplied no destination storage.
    EmptyBuffer,
    /// The ESP32-S3 DMA engine rejected or aborted the transfer.
    Dma(DmaError),
}

impl core::fmt::Display for CameraCaptureError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("camera receiver is busy"),
            Self::EmptyBuffer => formatter.write_str("camera destination buffer is empty"),
            Self::Dma(_) => formatter.write_str("camera DMA transfer failed"),
        }
    }
}

impl core::error::Error for CameraCaptureError {}

/// ESP32-S3 LCD_CAM receiver and its statically allocated DMA storage.
pub struct CameraReceiver {
    camera: Option<VendorCamera<'static>>,
    buffer: Option<DmaRxBuf>,
}

impl FrameReceiverErrorType for CameraReceiver {
    type Error = CameraCaptureError;
}

impl FrameReceiver for CameraReceiver {
    async fn receive(&mut self, frame: &mut [u8]) -> Result<usize, Self::Error> {
        if frame.is_empty() {
            return Err(CameraCaptureError::EmptyBuffer);
        }
        let Some(camera) = self.camera.take() else {
            return Err(CameraCaptureError::Busy);
        };
        let Some(mut buffer) = self.buffer.take() else {
            self.camera = Some(camera);
            return Err(CameraCaptureError::Busy);
        };
        buffer.set_length(frame.len().min(buffer.capacity()));

        let transfer = match camera.receive(buffer) {
            Ok(transfer) => transfer,
            Err((error, camera, buffer)) => {
                self.camera = Some(camera);
                self.buffer = Some(buffer);
                return Err(CameraCaptureError::Dma(error));
            }
        };
        let (result, camera, buffer) = transfer.wait();
        self.camera = Some(camera);
        let received = buffer.number_of_received_bytes().min(frame.len());
        buffer.read_received_data(&mut frame[..received]);
        self.buffer = Some(buffer);
        result.map_err(CameraCaptureError::Dma)?;
        Ok(received)
    }
}

/// Constructs the ESP32-S3 DVP receiver from Board-selected resources.
#[allow(clippy::too_many_arguments)]
pub fn camera_capture(
    controller: esp_hal::peripherals::LCD_CAM<'static>,
    dma: impl CamDmaRxChannel<'static>,
    xclk: impl PeripheralOutput<'static>,
    pclk: impl PeripheralInput<'static>,
    vsync: impl PeripheralInput<'static>,
    href: impl PeripheralInput<'static>,
    data0: impl PeripheralInput<'static>,
    data1: impl PeripheralInput<'static>,
    data2: impl PeripheralInput<'static>,
    data3: impl PeripheralInput<'static>,
    data4: impl PeripheralInput<'static>,
    data5: impl PeripheralInput<'static>,
    data6: impl PeripheralInput<'static>,
    data7: impl PeripheralInput<'static>,
    xclk_frequency_hz: u32,
    buffer: DmaRxBuf,
) -> Result<CameraReceiver, CameraConfigError> {
    let camera = VendorCamera::new(
        LcdCam::new(controller).cam,
        dma,
        VendorCameraConfig::default().with_frequency(Rate::from_hz(xclk_frequency_hz)),
    )
    .map_err(CameraConfigError::Peripheral)?
    .with_master_clock(xclk)
    .with_pixel_clock(pclk)
    .with_vsync(vsync)
    .with_h_enable(href)
    .with_data0(data0)
    .with_data1(data1)
    .with_data2(data2)
    .with_data3(data3)
    .with_data4(data4)
    .with_data5(data5)
    .with_data6(data6)
    .with_data7(data7);
    Ok(CameraReceiver {
        camera: Some(camera),
        buffer: Some(buffer),
    })
}

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

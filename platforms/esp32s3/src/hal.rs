//! ESP32-S3 adaptation from Board-selected tokens to hardware resources.

include!("../../esp32/src/hal.rs");

use barracuda_driver::{
    audio::{PcmFormat, PcmStream},
    camera::{FrameReceiver, FrameReceiverErrorType},
};
use esp_hal::{
    dma::{DmaBufError, DmaError, DmaRxBuf, DmaTxBuf},
    gpio::NoPin,
    i2s::master::{
        Channels, ConfigError as VendorI2sConfigError, DataFormat, Error as VendorI2sError, I2s,
        I2sMasterDmaChannel, I2sRx, I2sTx, Instance as I2sInstance, TdmConfig,
    },
    lcd_cam::{
        cam::{
            Camera as VendorCamera, Config as VendorCameraConfig,
            ConfigError as VendorCameraConfigError,
        },
        CamDmaRxChannel, LcdCam,
    },
};

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

#[doc(hidden)]
pub use __barracuda_esp32s3_camera_dma_buffer as camera_dma_buffer;
#[doc(hidden)]
pub use __barracuda_esp32s3_i2s_dma_buffers as i2s_dma_buffers;
#[doc(hidden)]
pub use __barracuda_esp32s3_i2s_dma_buffers as runtime_i2s_dma_buffers;

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

/// ESP32-S3 I2S construction failure.
#[derive(Debug)]
pub enum I2sConfigError {
    /// The Platform manifest did not provide an I2S DMA channel.
    MissingDma,
    /// The Board-selected DMA storage cannot be used by the controller.
    DmaBuffer(DmaBufError),
    /// Barracuda currently exposes signed 16-bit mono or stereo PCM only.
    UnsupportedFormat,
    /// The vendor HAL rejected the requested controller configuration.
    Peripheral(VendorI2sConfigError),
}

impl core::fmt::Display for I2sConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingDma => formatter.write_str("missing ESP32-S3 I2S DMA channel"),
            Self::DmaBuffer(_) => formatter.write_str("invalid I2S DMA buffer"),
            Self::UnsupportedFormat => formatter.write_str("unsupported I2S PCM format"),
            Self::Peripheral(_) => formatter.write_str("invalid ESP32-S3 I2S configuration"),
        }
    }
}

impl core::error::Error for I2sConfigError {}

/// Error surfaced while generated runtime I2S DMA storage is created.
pub type RuntimeI2sResourceError = I2sConfigError;

/// Platform-owned ESP32-S3 I2S controller, DMA channel, and bounded buffers.
pub struct RuntimeI2sResource {
    controller: esp_hal::peripherals::I2S0<'static>,
    dma: Option<esp_hal::peripherals::DMA_CH0<'static>>,
    tx_buffer: DmaTxBuf,
    rx_buffer: DmaRxBuf,
}

/// ESP32-S3 I2S transfer failure.
#[derive(Debug)]
pub enum I2sTransferError {
    /// A transfer is already in progress or the stream lost ownership state.
    Busy,
    /// The I2S peripheral rejected the transfer before DMA started.
    Peripheral(VendorI2sError),
    /// The DMA engine rejected or aborted the transfer.
    Dma(DmaError),
    /// DMA completed without producing every requested sample.
    ShortRead,
}

impl core::fmt::Display for I2sTransferError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("I2S stream is busy"),
            Self::Peripheral(_) => formatter.write_str("I2S transfer could not start"),
            Self::Dma(_) => formatter.write_str("I2S DMA transfer failed"),
            Self::ShortRead => formatter.write_str("I2S DMA transfer returned a short read"),
        }
    }
}

impl core::error::Error for I2sTransferError {}

/// ESP32-S3 full-duplex I2S stream and its Board-sized DMA storage.
pub struct I2sDevice {
    tx: Option<I2sTx<'static, Blocking>>,
    rx: Option<I2sRx<'static, Blocking>>,
    tx_buffer: Option<DmaTxBuf>,
    rx_buffer: Option<DmaRxBuf>,
    format: PcmFormat,
}

impl PcmStream for I2sDevice {
    type Error = I2sTransferError;

    fn format(&self) -> PcmFormat {
        self.format
    }

    async fn write(&mut self, samples: &[i16]) -> Result<(), Self::Error> {
        let chunk_samples = self
            .tx_buffer
            .as_ref()
            .map_or(0, |buffer| buffer.capacity() / 2);
        if chunk_samples == 0 {
            return Err(I2sTransferError::Busy);
        }
        for chunk in samples.chunks(chunk_samples) {
            let Some(tx) = self.tx.take() else {
                return Err(I2sTransferError::Busy);
            };
            let Some(mut buffer) = self.tx_buffer.take() else {
                self.tx = Some(tx);
                return Err(I2sTransferError::Busy);
            };
            for (sample, bytes) in chunk.iter().zip(buffer.as_mut_slice().chunks_exact_mut(2)) {
                bytes.copy_from_slice(&sample.to_le_bytes());
            }
            buffer.set_length(chunk.len() * 2);
            let transfer = match tx.write(buffer) {
                Ok(transfer) => transfer,
                Err((error, tx, buffer)) => {
                    self.tx = Some(tx);
                    self.tx_buffer = Some(buffer);
                    return Err(I2sTransferError::Peripheral(error));
                }
            };
            let (result, tx, buffer) = transfer.wait();
            self.tx = Some(tx);
            self.tx_buffer = Some(buffer);
            result.map_err(I2sTransferError::Dma)?;
        }
        Ok(())
    }

    async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
        let chunk_samples = self
            .rx_buffer
            .as_ref()
            .map_or(0, |buffer| buffer.capacity() / 2);
        if chunk_samples == 0 {
            return Err(I2sTransferError::Busy);
        }
        for chunk in samples.chunks_mut(chunk_samples) {
            let Some(rx) = self.rx.take() else {
                return Err(I2sTransferError::Busy);
            };
            let Some(mut buffer) = self.rx_buffer.take() else {
                self.rx = Some(rx);
                return Err(I2sTransferError::Busy);
            };
            buffer.set_length(chunk.len() * 2);
            let transfer = match rx.read(buffer) {
                Ok(transfer) => transfer,
                Err((error, rx, buffer)) => {
                    self.rx = Some(rx);
                    self.rx_buffer = Some(buffer);
                    return Err(I2sTransferError::Peripheral(error));
                }
            };
            let (result, rx, buffer) = transfer.wait();
            self.rx = Some(rx);
            result.map_err(I2sTransferError::Dma)?;
            if buffer.number_of_received_bytes() < chunk.len() * 2 {
                self.rx_buffer = Some(buffer);
                return Err(I2sTransferError::ShortRead);
            }
            for (sample, bytes) in chunk.iter_mut().zip(buffer.as_slice().chunks_exact(2)) {
                *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
            }
            self.rx_buffer = Some(buffer);
        }
        Ok(())
    }
}

/// Constructs a full-duplex ESP32-S3 I2S stream without an MCLK output.
#[allow(clippy::too_many_arguments)]
pub fn i2s_stream<I: I2sInstance + 'static>(
    controller: I,
    dma: impl I2sMasterDmaChannel<'static, I>,
    bclk: impl PeripheralOutput<'static>,
    ws: impl PeripheralOutput<'static>,
    dout: impl PeripheralOutput<'static>,
    din: impl PeripheralInput<'static>,
    sample_rate_hz: u32,
    channels: u8,
    bits_per_sample: u8,
    tx_buffer: DmaTxBuf,
    rx_buffer: DmaRxBuf,
) -> Result<I2sDevice, I2sConfigError> {
    let (config, format) = i2s_config(sample_rate_hz, channels, bits_per_sample, false)?;
    let i2s = I2s::new(controller, dma, config).map_err(I2sConfigError::Peripheral)?;
    finish_i2s(i2s, bclk, ws, dout, din, tx_buffer, rx_buffer, format)
}

/// Constructs a full-duplex ESP32-S3 I2S stream with an MCLK output.
#[allow(clippy::too_many_arguments)]
pub fn i2s_stream_with_mclk<I: I2sInstance + 'static>(
    controller: I,
    dma: impl I2sMasterDmaChannel<'static, I>,
    bclk: impl PeripheralOutput<'static>,
    ws: impl PeripheralOutput<'static>,
    dout: impl PeripheralOutput<'static>,
    din: impl PeripheralInput<'static>,
    mclk: impl PeripheralOutput<'static>,
    sample_rate_hz: u32,
    channels: u8,
    bits_per_sample: u8,
    tx_buffer: DmaTxBuf,
    rx_buffer: DmaRxBuf,
) -> Result<I2sDevice, I2sConfigError> {
    let (config, format) = i2s_config(sample_rate_hz, channels, bits_per_sample, true)?;
    let i2s = I2s::new(controller, dma, config)
        .map_err(I2sConfigError::Peripheral)?
        .with_mclk(mclk);
    finish_i2s(i2s, bclk, ws, dout, din, tx_buffer, rx_buffer, format)
}

fn i2s_config(
    sample_rate_hz: u32,
    channels: u8,
    bits_per_sample: u8,
    has_master_clock: bool,
) -> Result<(TdmConfig, PcmFormat), I2sConfigError> {
    let channels_config = match channels {
        1 => Channels::MONO,
        2 => Channels::STEREO,
        _ => return Err(I2sConfigError::UnsupportedFormat),
    };
    if bits_per_sample != 16 {
        return Err(I2sConfigError::UnsupportedFormat);
    }
    Ok((
        TdmConfig::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(sample_rate_hz))
            .with_data_format(DataFormat::Data16Channel16)
            .with_channels(channels_config),
        PcmFormat {
            sample_rate_hz,
            channels,
            bits_per_sample,
            master_clock_hz: has_master_clock.then_some(sample_rate_hz.saturating_mul(256)),
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn finish_i2s(
    i2s: I2s<'static, Blocking>,
    bclk: impl PeripheralOutput<'static>,
    ws: impl PeripheralOutput<'static>,
    dout: impl PeripheralOutput<'static>,
    din: impl PeripheralInput<'static>,
    tx_buffer: DmaTxBuf,
    rx_buffer: DmaRxBuf,
    format: PcmFormat,
) -> Result<I2sDevice, I2sConfigError> {
    if tx_buffer.capacity() < 2 || rx_buffer.capacity() < 2 {
        return Err(I2sConfigError::UnsupportedFormat);
    }
    let tx = i2s
        .i2s_tx
        .with_bclk(bclk)
        .with_ws(ws)
        .with_dout(dout)
        .build();
    let rx = i2s.i2s_rx.with_din(din).build();
    Ok(I2sDevice {
        tx: Some(tx),
        rx: Some(rx),
        tx_buffer: Some(tx_buffer),
        rx_buffer: Some(rx_buffer),
        format,
    })
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

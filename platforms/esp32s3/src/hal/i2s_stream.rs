//! Full-duplex I2S PCM streams through esp-hal's I2S master driver, shared
//! by the ESP32, ESP32-S3, and ESP32-P4 Platforms (the ESP32 and ESP32-P4
//! include this file). The ESP32 routes its master clock only to the fixed
//! `CLK_OUT` pins, so it builds streams without one (`fixed-mclk-pins`).

use barracuda_peripheral::audio::{PcmFormat, PcmStream};
use esp_hal::{
    dma::{DmaBufError, DmaError, DmaRxBuf, DmaTxBuf},
    gpio::interconnect::{PeripheralInput, PeripheralOutput},
    i2s::master::{
        Channels, ConfigError as VendorI2sConfigError, DataFormat, Error as VendorI2sError, I2s,
        I2sMasterDmaChannel, I2sRx, I2sTx, Instance as I2sInstance, TdmConfig,
    },
    time::Rate,
    Blocking,
};

/// ESP I2S construction failure.
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
            Self::MissingDma => formatter.write_str("missing ESP I2S DMA channel"),
            Self::DmaBuffer(_) => formatter.write_str("invalid I2S DMA buffer"),
            Self::UnsupportedFormat => formatter.write_str("unsupported I2S PCM format"),
            Self::Peripheral(_) => formatter.write_str("invalid ESP I2S configuration"),
        }
    }
}

impl core::error::Error for I2sConfigError {}

/// ESP I2S transfer failure.
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
    /// The stream has no lines for this direction.
    Unsupported,
    /// The controller rejected the mode this transfer needs.
    Configuration(VendorI2sConfigError),
}

impl core::fmt::Display for I2sTransferError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("I2S stream is busy"),
            Self::Peripheral(_) => formatter.write_str("I2S transfer could not start"),
            Self::Dma(_) => formatter.write_str("I2S DMA transfer failed"),
            Self::ShortRead => formatter.write_str("I2S DMA transfer returned a short read"),
            Self::Unsupported => {
                formatter.write_str("the I2S stream has no lines for this direction")
            }
            Self::Configuration(_) => formatter.write_str("invalid ESP I2S configuration"),
        }
    }
}

impl core::error::Error for I2sTransferError {}

/// Full-duplex I2S stream and its Board-sized DMA storage.
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
            for (sample, bytes) in chunk
                .iter()
                .zip(buffer.as_mut_slice().as_chunks_mut::<2>().0)
            {
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
            for (sample, bytes) in chunk.iter_mut().zip(buffer.as_slice().as_chunks::<2>().0) {
                *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
            }
            self.rx_buffer = Some(buffer);
        }
        Ok(())
    }
}

/// Constructs a full-duplex ESP I2S stream without an MCLK output.
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

/// Constructs a full-duplex ESP I2S stream with an MCLK output.
#[cfg(not(feature = "fixed-mclk-pins"))]
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

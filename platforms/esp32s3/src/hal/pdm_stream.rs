//! A PDM microphone and an I2S amplifier sharing one I2S controller, shared
//! by the ESP32 and ESP32-S3 Platforms (the ESP32 includes this file).
//!
//! M5Stack wires the microphone's clock and the amplifier's word select to
//! one pin (Core2: G0, Atom Echo: G33, Cardputer: G43), as M5Unified's
//! `Mic_Class` and `Speaker_Class` assume: only one runs at a time. Each
//! transfer therefore builds the controller in the mode it needs from the
//! reborrowed controller, DMA channel, and pins: PDM receive with the
//! hardware PDM-to-PCM filter for a read, standard (Philips) I2S transmit for
//! a write. Only `I2S0` has the PDM receiver on both chips.

use barracuda_peripheral::audio::{PcmFormat, PcmStream};
use esp_hal::{
    dma::{DmaRxBuf, DmaTxBuf},
    gpio::AnyPin,
    i2s::{
        master::{Channels, DataFormat, I2s, TdmConfig},
        pdm::{PdmConfig, PdmRxConfig, PdmSlotMode},
    },
    peripherals::I2S0,
    time::Rate,
};

use super::{I2sConfigError, I2sTransferError, PdmDmaChannel};

/// Bytes of one stereo 16-bit frame the amplifier receives per mono sample.
const TX_FRAME_BYTES: usize = 4;

/// One I2S controller time-shared by a PDM microphone and an I2S amplifier.
///
/// The stream carries mono 16-bit PCM. Writes go to both I2S channels, so an
/// amplifier that plays either channel (the NS4168 picks one with its CTRL
/// pin) plays every sample.
pub struct PdmI2sStream {
    controller: I2S0<'static>,
    dma: PdmDmaChannel<'static>,
    clk: AnyPin<'static>,
    din: Option<AnyPin<'static>>,
    bclk: Option<AnyPin<'static>>,
    dout: Option<AnyPin<'static>>,
    tx_buffer: Option<DmaTxBuf>,
    rx_buffer: Option<DmaRxBuf>,
    format: PcmFormat,
}

/// Builds the time-shared stream from Board-selected resources. `clk` is the
/// microphone clock and the amplifier word select; a microphone-only Board
/// passes no `bclk` and `dout`, an amplifier-only one no `din`.
///
/// # Errors
///
/// Returns [`I2sConfigError::UnsupportedFormat`] when neither direction is
/// wired, the amplifier has only one of its lines, or a buffer cannot hold
/// one frame.
#[allow(clippy::too_many_arguments)]
pub fn pdm_i2s_stream(
    controller: I2S0<'static>,
    dma: impl Into<PdmDmaChannel<'static>>,
    clk: AnyPin<'static>,
    din: Option<AnyPin<'static>>,
    bclk: Option<AnyPin<'static>>,
    dout: Option<AnyPin<'static>>,
    sample_rate_hz: u32,
    tx_buffer: DmaTxBuf,
    rx_buffer: DmaRxBuf,
) -> Result<PdmI2sStream, I2sConfigError> {
    if bclk.is_some() != dout.is_some()
        || (din.is_none() && dout.is_none())
        || tx_buffer.capacity() < TX_FRAME_BYTES
        || rx_buffer.capacity() < 2
    {
        return Err(I2sConfigError::UnsupportedFormat);
    }
    Ok(PdmI2sStream {
        controller,
        dma: dma.into(),
        clk,
        din,
        bclk,
        dout,
        tx_buffer: Some(tx_buffer),
        rx_buffer: Some(rx_buffer),
        format: PcmFormat {
            sample_rate_hz,
            channels: 1,
            bits_per_sample: 16,
            master_clock_hz: None,
        },
    })
}

impl PcmStream for PdmI2sStream {
    type Error = I2sTransferError;

    fn format(&self) -> PcmFormat {
        self.format
    }

    async fn write(&mut self, samples: &[i16]) -> Result<(), Self::Error> {
        let (Some(bclk), Some(dout)) = (self.bclk.as_mut(), self.dout.as_mut()) else {
            return Err(I2sTransferError::Unsupported);
        };
        let Some(mut buffer) = self.tx_buffer.take() else {
            return Err(I2sTransferError::Busy);
        };
        let config = TdmConfig::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(self.format.sample_rate_hz))
            .with_data_format(DataFormat::Data16Channel16)
            .with_channels(Channels::STEREO);
        let i2s = match I2s::new(self.controller.reborrow(), self.dma.reborrow(), config) {
            Ok(i2s) => i2s,
            Err(error) => {
                self.tx_buffer = Some(buffer);
                return Err(I2sTransferError::Configuration(error));
            }
        };
        let mut tx = i2s
            .i2s_tx
            .with_bclk(bclk.reborrow())
            .with_ws(self.clk.reborrow())
            .with_dout(dout.reborrow())
            .build();
        let chunk_samples = buffer.capacity() / TX_FRAME_BYTES;
        for chunk in samples.chunks(chunk_samples) {
            for (sample, frame) in chunk
                .iter()
                .zip(buffer.as_mut_slice().as_chunks_mut::<TX_FRAME_BYTES>().0)
            {
                let bytes = sample.to_le_bytes();
                *frame = [bytes[0], bytes[1], bytes[0], bytes[1]];
            }
            buffer.set_length(chunk.len() * TX_FRAME_BYTES);
            let transfer = match tx.write(buffer) {
                Ok(transfer) => transfer,
                Err((error, _, returned)) => {
                    self.tx_buffer = Some(returned);
                    return Err(I2sTransferError::Peripheral(error));
                }
            };
            let (result, returned_tx, returned) = transfer.wait();
            tx = returned_tx;
            buffer = returned;
            if let Err(error) = result {
                self.tx_buffer = Some(buffer);
                return Err(I2sTransferError::Dma(error));
            }
        }
        self.tx_buffer = Some(buffer);
        Ok(())
    }

    async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
        let Some(din) = self.din.as_mut() else {
            return Err(I2sTransferError::Unsupported);
        };
        let Some(mut buffer) = self.rx_buffer.take() else {
            return Err(I2sTransferError::Busy);
        };
        let config = PdmConfig::rx_only(PdmRxConfig::new_pcm_default(
            Rate::from_hz(self.format.sample_rate_hz),
            PdmSlotMode::Mono,
        ));
        let i2s = match I2s::new_pdm(self.controller.reborrow(), self.dma.reborrow(), config) {
            Ok(i2s) => i2s,
            Err(error) => {
                self.rx_buffer = Some(buffer);
                return Err(I2sTransferError::Configuration(error));
            }
        };
        let mut rx = i2s
            .i2s_rx
            .with_clk(self.clk.reborrow())
            .with_din(din.reborrow())
            .build();
        let chunk_samples = buffer.capacity() / 2;
        for chunk in samples.chunks_mut(chunk_samples) {
            buffer.set_length(chunk.len() * 2);
            let transfer = match rx.read(buffer) {
                Ok(transfer) => transfer,
                Err((error, _, returned)) => {
                    self.rx_buffer = Some(returned);
                    return Err(I2sTransferError::Peripheral(error));
                }
            };
            let (result, returned_rx, returned) = transfer.wait();
            rx = returned_rx;
            buffer = returned;
            if let Err(error) = result {
                self.rx_buffer = Some(buffer);
                return Err(I2sTransferError::Dma(error));
            }
            if buffer.number_of_received_bytes() < chunk.len() * 2 {
                self.rx_buffer = Some(buffer);
                return Err(I2sTransferError::ShortRead);
            }
            for (sample, bytes) in chunk.iter_mut().zip(buffer.as_slice().as_chunks::<2>().0) {
                *sample = i16::from_le_bytes(*bytes);
            }
        }
        self.rx_buffer = Some(buffer);
        Ok(())
    }
}

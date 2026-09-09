//! ES8389 audio codec implementation using embedded-hal I2C and a PCM binding.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_es8389::Es8389;
use barracuda_peripheral::{
    PeripheralImplementation,
    audio::{AudioCodec, AudioDescriptor, PcmStream},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

/// Board-owned ES8389 configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Es8389Config {
    address: u8,
}

impl Es8389Config {
    /// Creates configuration for one seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self { address }
    }
}

/// Move-only resources consumed by the codec implementation.
pub struct Es8389Bindings<I2C, I2S, DELAY> {
    control: I2C,
    stream: I2S,
    delay: DELAY,
}

impl<I2C, I2S, DELAY> Es8389Bindings<I2C, I2S, DELAY> {
    /// Combines codec control, PCM stream, and initialization delay.
    #[must_use]
    pub const fn new(control: I2C, stream: I2S, delay: DELAY) -> Self {
        Self {
            control,
            stream,
            delay,
        }
    }
}

/// ES8389 initialization failure.
#[derive(Debug)]
pub enum Es8389InitError<ControlError> {
    /// This implementation currently supports 16 kHz, stereo, signed 16-bit PCM.
    UnsupportedFormat,
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress,
    /// Codec register programming failed.
    Control(ControlError),
}

/// ES8389 control or stream failure.
#[derive(Debug)]
pub enum Es8389Error<ControlError, StreamError> {
    /// Codec register access failed.
    Control(ControlError),
    /// I2S transfer failed.
    Stream(StreamError),
}

/// Initialized ES8389 codec and full-duplex PCM stream.
pub struct Es8389AudioCodec<I2C, I2S> {
    control: I2C,
    stream: I2S,
    chip: Es8389,
    descriptor: AudioDescriptor,
}

/// Static factory used by generated Board composition.
pub struct Es8389AudioCodecImplementation<I2C, I2S, DELAY>(
    PhantomData<I2C>,
    PhantomData<I2S>,
    PhantomData<DELAY>,
);

impl<I2C, I2S, DELAY> PeripheralImplementation for Es8389AudioCodecImplementation<I2C, I2S, DELAY>
where
    I2C: I2c + 'static,
    I2S: PcmStream + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Es8389Bindings<I2C, I2S, DELAY>;
    type Config = Es8389Config;
    type Peripheral = Es8389AudioCodec<I2C, I2S>;
    type Error = Es8389InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let format = bindings.stream.format();
        if format.sample_rate_hz != 16_000 || format.channels != 2 || format.bits_per_sample != 16 {
            return Err(Es8389InitError::UnsupportedFormat);
        }

        let chip = Es8389::new(config.address).ok_or(Es8389InitError::InvalidAddress)?;
        chip.initialize(
            &mut bindings.control,
            format.master_clock_hz.is_none(),
            &mut bindings.delay,
        )
        .map_err(Es8389InitError::Control)?;

        Ok(Es8389AudioCodec {
            control: bindings.control,
            stream: bindings.stream,
            chip,
            descriptor: AudioDescriptor::new(
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
            ),
        })
    }
}

impl<I2C, I2S> AudioCodec for Es8389AudioCodec<I2C, I2S>
where
    I2C: I2c,
    I2S: PcmStream,
{
    type Error = Es8389Error<I2C::Error, I2S::Error>;

    fn descriptor(&self) -> AudioDescriptor {
        self.descriptor
    }

    fn set_output_volume(&mut self, volume: u8) -> Result<(), Self::Error> {
        self.chip
            .set_dac_volume(&mut self.control, volume)
            .map_err(Es8389Error::Control)
    }

    async fn write(&mut self, samples: &[i16]) -> Result<(), Self::Error> {
        self.stream
            .write(samples)
            .await
            .map_err(Es8389Error::Stream)
    }

    async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
        self.stream.read(samples).await.map_err(Es8389Error::Stream)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::{convert::Infallible, future::ready};

    use barracuda_peripheral::{
        PeripheralImplementation,
        audio::{AudioCodec, PcmFormat, PcmStream},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{Es8389AudioCodecImplementation, Es8389Bindings, Es8389Config};

    #[derive(Default)]
    struct ControlBus {
        writes: std::vec::Vec<[u8; 2]>,
    }

    impl ErrorType for ControlBus {
        type Error = Infallible;
    }

    impl I2c for ControlBus {
        fn read(&mut self, _: u8, _: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn write(&mut self, _: u8, bytes: &[u8]) -> Result<(), Self::Error> {
            self.writes.push([bytes[0], bytes[1]]);
            Ok(())
        }

        fn write_read(&mut self, _: u8, _: &[u8], _: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn transaction(&mut self, _: u8, _: &mut [Operation<'_>]) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay;

    impl DelayNs for Delay {
        fn delay_ns(&mut self, _: u32) {}
    }

    struct Stream {
        written: std::vec::Vec<i16>,
    }

    impl PcmStream for Stream {
        type Error = Infallible;

        fn format(&self) -> PcmFormat {
            PcmFormat {
                sample_rate_hz: 16_000,
                channels: 2,
                bits_per_sample: 16,
                master_clock_hz: None,
            }
        }

        fn write<'a>(
            &'a mut self,
            samples: &'a [i16],
        ) -> impl core::future::Future<Output = Result<(), Self::Error>> + 'a {
            self.written.extend(samples);
            ready(Ok(()))
        }

        fn read<'a>(
            &'a mut self,
            samples: &'a mut [i16],
        ) -> impl core::future::Future<Output = Result<(), Self::Error>> + 'a {
            samples.fill(0x1234);
            ready(Ok(()))
        }
    }

    #[test]
    fn initializes_bclk_clock_path_and_streams_pcm() {
        let bindings = Es8389Bindings::new(
            ControlBus::default(),
            Stream {
                written: std::vec::Vec::new(),
            },
            Delay,
        );
        let mut codec = block_on(Es8389AudioCodecImplementation::initialize(
            bindings,
            Es8389Config::new(0x20),
        ))
        .expect("initialize codec");
        assert_eq!(codec.descriptor().sample_rate_hz(), 16_000);
        block_on(codec.write(&[-1, 2])).expect("write PCM");
        let mut samples = [0_i16; 2];
        block_on(codec.read(&mut samples)).expect("read PCM");
        assert_eq!(samples, [0x1234, 0x1234]);
        codec.set_output_volume(128).expect("set volume");
    }
}

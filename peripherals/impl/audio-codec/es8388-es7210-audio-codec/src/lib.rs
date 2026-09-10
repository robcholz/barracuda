//! Full-duplex audio implementation for an ES8388 DAC and ES7210 microphone ADC.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_es7210::Es7210;
use barracuda_driver_es8388::Es8388;
use barracuda_driver_pi4ioe5v6408::{Error as ExpanderError, Pi4ioe5v6408};
use barracuda_peripheral::{
    PeripheralImplementation,
    audio::{AudioCodec, AudioDescriptor, PcmStream},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

const CONTROL_EXPANDER_ADDRESS: u8 = 0x43;
const SPEAKER_ENABLE_PIN: u8 = 1;

/// Board-owned addresses for the paired codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Es8388Es7210Config {
    output_address: u8,
    input_address: u8,
}

impl Es8388Es7210Config {
    /// Creates a paired-codec configuration from two seven-bit I2C addresses.
    #[must_use]
    pub const fn new(output_address: u8, input_address: u8) -> Self {
        Self {
            output_address,
            input_address,
        }
    }
}

/// Move-only resources consumed by the paired audio implementation.
pub struct Es8388Es7210Bindings<I2C, I2S, DELAY> {
    control: I2C,
    stream: I2S,
    delay: DELAY,
}

impl<I2C, I2S, DELAY> Es8388Es7210Bindings<I2C, I2S, DELAY> {
    /// Combines the shared control bus, PCM stream, and initialization delay.
    #[must_use]
    pub const fn new(control: I2C, stream: I2S, delay: DELAY) -> Self {
        Self {
            control,
            stream,
            delay,
        }
    }
}

/// Paired-codec initialization failure.
#[derive(Debug)]
pub enum Es8388Es7210InitError<ControlError> {
    /// Tab5 audio requires 48 kHz, stereo, signed 16-bit PCM and 256-fs MCLK.
    UnsupportedFormat,
    /// One codec address is not a seven-bit I2C address.
    InvalidAddress,
    /// Codec register programming failed.
    Control(ControlError),
    /// The private Tab5 speaker-enable expander could not be configured.
    Expander(ExpanderError<ControlError>),
}

/// Paired-codec control or stream failure.
#[derive(Debug)]
pub enum Es8388Es7210Error<ControlError, StreamError> {
    /// Codec register access failed.
    Control(ControlError),
    /// I2S transfer failed.
    Stream(StreamError),
}

/// Initialized ES8388 output, ES7210 input, and full-duplex PCM stream.
pub struct Es8388Es7210AudioCodec<I2C, I2S> {
    control: I2C,
    stream: I2S,
    output: Es8388,
    descriptor: AudioDescriptor,
}

/// Static factory used by generated Board composition.
pub struct Es8388Es7210AudioCodecImplementation<I2C, I2S, DELAY>(
    PhantomData<I2C>,
    PhantomData<I2S>,
    PhantomData<DELAY>,
);

impl<I2C, I2S, DELAY> PeripheralImplementation
    for Es8388Es7210AudioCodecImplementation<I2C, I2S, DELAY>
where
    I2C: I2c + 'static,
    I2S: PcmStream + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Es8388Es7210Bindings<I2C, I2S, DELAY>;
    type Config = Es8388Es7210Config;
    type Peripheral = Es8388Es7210AudioCodec<I2C, I2S>;
    type Error = Es8388Es7210InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let format = bindings.stream.format();
        if format.sample_rate_hz != 48_000
            || format.channels != 2
            || format.bits_per_sample != 16
            || format.master_clock_hz != Some(12_288_000)
        {
            return Err(Es8388Es7210InitError::UnsupportedFormat);
        }

        let output =
            Es8388::new(config.output_address).ok_or(Es8388Es7210InitError::InvalidAddress)?;
        let input =
            Es7210::new(config.input_address).ok_or(Es8388Es7210InitError::InvalidAddress)?;
        Pi4ioe5v6408::new(&mut bindings.control, CONTROL_EXPANDER_ADDRESS)
            .map_err(Es8388Es7210InitError::Expander)?
            .configure_output(SPEAKER_ENABLE_PIN, true)
            .map_err(Es8388Es7210InitError::Expander)?;
        output
            .initialize(&mut bindings.control)
            .map_err(Es8388Es7210InitError::Control)?;
        bindings.delay.delay_ms(10);
        input
            .initialize(&mut bindings.control)
            .map_err(Es8388Es7210InitError::Control)?;
        bindings.delay.delay_ms(10);

        Ok(Es8388Es7210AudioCodec {
            control: bindings.control,
            stream: bindings.stream,
            output,
            descriptor: AudioDescriptor::new(
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
            ),
        })
    }
}

impl<I2C, I2S> AudioCodec for Es8388Es7210AudioCodec<I2C, I2S>
where
    I2C: I2c,
    I2S: PcmStream,
{
    type Error = Es8388Es7210Error<I2C::Error, I2S::Error>;

    fn descriptor(&self) -> AudioDescriptor {
        self.descriptor
    }

    fn set_output_volume(&mut self, volume: u8) -> Result<(), Self::Error> {
        let attenuation = 0xc0_u16.saturating_sub(u16::from(volume) * 0xc0 / 0xff) as u8;
        self.output
            .set_dac_attenuation(&mut self.control, attenuation)
            .map_err(Es8388Es7210Error::Control)
    }

    async fn write(&mut self, samples: &[i16]) -> Result<(), Self::Error> {
        self.stream
            .write(samples)
            .await
            .map_err(Es8388Es7210Error::Stream)
    }

    async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
        self.stream
            .read(samples)
            .await
            .map_err(Es8388Es7210Error::Stream)
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

    use super::{Es8388Es7210AudioCodecImplementation, Es8388Es7210Bindings, Es8388Es7210Config};

    #[derive(Default)]
    struct ControlBus {
        writes: std::vec::Vec<(u8, [u8; 2])>,
    }

    impl ErrorType for ControlBus {
        type Error = Infallible;
    }

    impl I2c for ControlBus {
        fn read(&mut self, _: u8, bytes: &mut [u8]) -> Result<(), Self::Error> {
            bytes.fill(0);
            Ok(())
        }

        fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), Self::Error> {
            self.writes.push((address, [bytes[0], bytes[1]]));
            Ok(())
        }

        fn write_read(&mut self, _: u8, _: &[u8], bytes: &mut [u8]) -> Result<(), Self::Error> {
            bytes.fill(0);
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

    struct Stream;

    impl PcmStream for Stream {
        type Error = Infallible;

        fn format(&self) -> PcmFormat {
            PcmFormat {
                sample_rate_hz: 48_000,
                channels: 2,
                bits_per_sample: 16,
                master_clock_hz: Some(12_288_000),
            }
        }

        fn write<'a>(
            &'a mut self,
            _: &'a [i16],
        ) -> impl core::future::Future<Output = Result<(), Self::Error>> + 'a {
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
    fn initializes_expander_output_and_both_codecs() {
        let bindings = Es8388Es7210Bindings::new(ControlBus::default(), Stream, Delay);
        let mut codec = block_on(Es8388Es7210AudioCodecImplementation::initialize(
            bindings,
            Es8388Es7210Config::new(0x10, 0x40),
        ))
        .expect("initialize paired codecs");
        assert_eq!(codec.descriptor().sample_rate_hz(), 48_000);
        assert_eq!(
            &codec.control.writes[..3],
            &[
                (0x43, [0x03, 0x02]),
                (0x43, [0x07, 0x00]),
                (0x43, [0x05, 0x02]),
            ]
        );
        assert!(
            codec
                .control
                .writes
                .iter()
                .any(|(address, _)| *address == 0x10)
        );
        assert!(
            codec
                .control
                .writes
                .iter()
                .any(|(address, _)| *address == 0x40)
        );
        let mut samples = [0_i16; 2];
        block_on(codec.read(&mut samples)).expect("read PCM");
        assert_eq!(samples, [0x1234, 0x1234]);
        codec.set_output_volume(255).expect("set volume");
    }
}

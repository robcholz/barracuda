//! ES8389 audio codec Driver using embedded-hal I2C and a PCM binding.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver::{
    PeripheralDriver,
    audio::{AudioCodec, AudioDescriptor, PcmStream},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

// Ported from Espressif's Apache-2.0 esp_codec_dev 1.6.2 ES8389 open
// sequence. The codec stays in I2S slave mode; the Platform owns all clocks.
const INITIAL_REGISTERS: &[(u8, u8)] = &[
    (0xf3, 0x00),
    (0x00, 0x7e),
    (0xf3, 0x38),
    (0x24, 0x64),
    (0x25, 0x04),
    (0x45, 0x03),
    (0x60, 0x2a),
    (0x61, 0xc9),
    (0x62, 0x4f),
    (0x63, 0x06),
    (0x6b, 0x00),
    (0x6d, 0x16),
    (0x6e, 0xaa),
    (0x6f, 0x66),
    (0x70, 0x99),
    (0x23, 0x00),
    (0x72, 0x10),
    (0x73, 0x10),
    (0x10, 0xc4),
    (0x01, 0x08),
    (0xf1, 0x00),
    (0x12, 0x01),
    (0x13, 0x01),
    (0x14, 0x01),
    (0x15, 0x01),
    (0x16, 0x35),
    (0x17, 0x09),
    (0x18, 0x91),
    (0x19, 0x28),
    (0x1a, 0x01),
    (0x1b, 0x01),
    (0x1c, 0x11),
    (0x2a, 0x00),
    (0x20, 0x60),
    (0x40, 0x60),
    (0xf0, 0x01),
    (0x02, 0x00),
    (0x04, 0x00),
    (0x05, 0x10),
    (0x06, 0x00),
    (0x07, 0xc0),
    (0x08, 0x00),
    (0x09, 0xc0),
    (0x0a, 0x80),
    (0x0b, 0x04),
    (0x0c, 0x01),
    (0x0d, 0x00),
    (0x0f, 0x10),
    (0x21, 0x1f),
    (0x22, 0x7f),
    (0x2f, 0xc0),
    (0x30, 0xf4),
    (0x31, 0x00),
    (0x44, 0x00),
    (0x41, 0x7f),
    (0x42, 0x7f),
    (0x43, 0x10),
    (0x49, 0x0f),
    (0x4c, 0xc0),
    (0x00, 0x00),
    (0x03, 0xc1),
    (0x00, 0x01),
    (0x4d, 0x00),
    (0x26, 0xbf),
    (0x27, 0xbf),
    (0x28, 0xbf),
    (0x46, 0xbf),
    (0x47, 0xbf),
    (0x48, 0xbe),
    // Internal ADCL/DACR reference signal used by Espressif's default path.
    (0x23, 0x80),
    (0xf0, 0x1a),
];

// Official 16 kHz, 64-fs clock coefficients used when no external MCLK is
// wired and ES8389 derives its internal clock from BCLK.
const BCLK_16_KHZ_REGISTERS: &[(u8, u8)] = &[
    (0x04, 0x00),
    (0x05, 0x45),
    (0x06, 0x24),
    (0x07, 0xc0),
    (0x08, 0x01),
    (0x09, 0xd1),
    (0x0a, 0x90),
    (0x0f, 0x10),
    (0x11, 0x00),
    (0x21, 0x1f),
    (0x22, 0x7f),
    (0x26, 0xbf),
    (0x30, 0xf4),
    (0x41, 0xff),
    (0x42, 0x7f),
    (0x43, 0x11),
    (0xf0, 0x1a),
    (0xf1, 0x00),
    (0x16, 0x12),
    (0x18, 0x31),
    (0x19, 0x0e),
];

const BIAS_ON_REGISTERS: &[(u8, u8)] = &[
    (0x4d, 0x00),
    (0x69, 0x20),
    (0x61, 0xd9),
    (0x64, 0x8f),
    (0x10, 0xe4),
    (0x00, 0x01),
    (0x03, 0xc3),
    (0x24, 0x6a),
    (0x25, 0x0a),
];

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

/// Move-only resources consumed by the codec Driver.
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
    /// This Driver currently supports 16 kHz, stereo, signed 16-bit PCM.
    UnsupportedFormat,
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
    address: u8,
    descriptor: AudioDescriptor,
}

/// Static factory used by generated Board composition.
pub struct Es8389AudioCodecDriver<I2C, I2S, DELAY>(
    PhantomData<I2C>,
    PhantomData<I2S>,
    PhantomData<DELAY>,
);

impl<I2C, I2S, DELAY> PeripheralDriver for Es8389AudioCodecDriver<I2C, I2S, DELAY>
where
    I2C: I2c + 'static,
    I2S: PcmStream + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Es8389Bindings<I2C, I2S, DELAY>;
    type Config = Es8389Config;
    type Capability = Es8389AudioCodec<I2C, I2S>;
    type Error = Es8389InitError<I2C::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Capability, Self::Error> {
        let format = bindings.stream.format();
        if format.sample_rate_hz != 16_000 || format.channels != 2 || format.bits_per_sample != 16 {
            return Err(Es8389InitError::UnsupportedFormat);
        }

        write_sequence(&mut bindings.control, config.address, INITIAL_REGISTERS)
            .map_err(Es8389InitError::Control)?;
        bindings.delay.delay_ms(10);
        if format.master_clock_hz.is_none() {
            write_register(&mut bindings.control, config.address, 0x02, 0x40)
                .map_err(Es8389InitError::Control)?;
            write_sequence(&mut bindings.control, config.address, BCLK_16_KHZ_REGISTERS)
                .map_err(Es8389InitError::Control)?;
        }
        write_sequence(&mut bindings.control, config.address, BIAS_ON_REGISTERS)
            .map_err(Es8389InitError::Control)?;

        Ok(Es8389AudioCodec {
            control: bindings.control,
            stream: bindings.stream,
            address: config.address,
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
        write_register(&mut self.control, self.address, 0x46, volume)
            .map_err(Es8389Error::Control)?;
        write_register(&mut self.control, self.address, 0x47, volume).map_err(Es8389Error::Control)
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

fn write_sequence<I2C: I2c>(
    i2c: &mut I2C,
    address: u8,
    sequence: &[(u8, u8)],
) -> Result<(), I2C::Error> {
    for &(register, value) in sequence {
        write_register(i2c, address, register, value)?;
    }
    Ok(())
}

fn write_register<I2C: I2c>(
    i2c: &mut I2C,
    address: u8,
    register: u8,
    value: u8,
) -> Result<(), I2C::Error> {
    i2c.write(address, &[register, value])
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::{convert::Infallible, future::ready};

    use barracuda_driver::{
        PeripheralDriver,
        audio::{AudioCodec, PcmFormat, PcmStream},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{Es8389AudioCodecDriver, Es8389Bindings, Es8389Config};

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
        let mut codec = block_on(Es8389AudioCodecDriver::initialize(
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

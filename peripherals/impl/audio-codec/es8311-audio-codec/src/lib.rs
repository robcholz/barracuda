//! ES8311 audio codec implementation using embedded-hal I2C and a PCM binding.
//!
//! The speaker amplifier's enable is a GPIO, a pin of M5Stack's M5IOE1 or
//! M5PM1 helper chip on the codec's control bus, or absent (the
//! Cardputer-ADV's NS4150B runs whenever it is powered). The codec takes its
//! master clock from its MCLK pin or, where none is wired, from the bit
//! clock.

#![no_std]

mod enables;

use core::marker::PhantomData;

pub use barracuda_driver_es8311::ClockSource;
use barracuda_driver_es8311::{Es8311, Es8311Error as Es8311RegisterError};
use barracuda_peripheral::{
    PeripheralImplementation,
    audio::{AudioCodec, AudioDescriptor, PcmStream},
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c};
pub use enables::{HelperEnables, HelperError};

/// Board-owned ES8311 configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Es8311Config {
    address: u8,
    clock: ClockSource,
    enables: HelperEnables,
}

impl Es8311Config {
    /// Creates configuration for one seven-bit SCCB/I2C address, clocked
    /// from the MCLK pin, with no helper-chip enables.
    #[must_use]
    pub const fn new(address: u8) -> Self {
        Self {
            address,
            clock: ClockSource::Mclk,
            enables: HelperEnables::new(0, 0, -1),
        }
    }

    /// Takes the master clock from `clock`.
    #[must_use]
    pub const fn with_clock(mut self, clock: ClockSource) -> Self {
        self.clock = clock;
        self
    }

    /// Switches the audio supply and amplifier through helper-chip pins.
    #[must_use]
    pub const fn with_helper_enables(mut self, enables: HelperEnables) -> Self {
        self.enables = enables;
        self
    }
}

/// Move-only resources consumed by the codec implementation.
pub struct Es8311Bindings<I2C, I2S, AMP, DELAY> {
    control: I2C,
    stream: I2S,
    amplifier_enable: Option<AMP>,
    delay: DELAY,
}

impl<I2C, I2S, AMP, DELAY> Es8311Bindings<I2C, I2S, AMP, DELAY> {
    /// Combines the codec control bus, PCM stream, amplifier, and reset delay.
    #[must_use]
    pub const fn new(
        control: I2C,
        stream: I2S,
        amplifier_enable: Option<AMP>,
        delay: DELAY,
    ) -> Self {
        Self {
            control,
            stream,
            amplifier_enable,
            delay,
        }
    }
}

/// ES8311 initialization failure.
#[derive(Debug)]
pub enum Es8311InitError<ControlError, AmplifierError> {
    /// The configured I2S format is not 16-bit stereo PCM.
    UnsupportedFormat,
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress,
    /// Codec register programming failed.
    Control(Es8311RegisterError<ControlError>),
    /// The external speaker amplifier could not be enabled.
    Amplifier(AmplifierError),
    /// A helper chip switching the audio supply or amplifier failed.
    Helper(HelperError<ControlError>),
}

/// ES8311 control or stream failure.
#[derive(Debug)]
pub enum Es8311Error<ControlError, StreamError> {
    /// Codec register access failed.
    Control(Es8311RegisterError<ControlError>),
    /// I2S transfer failed.
    Stream(StreamError),
}

/// Initialized ES8311 codec and its full-duplex PCM stream.
pub struct Es8311AudioCodec<I2C, I2S, AMP> {
    control: I2C,
    stream: I2S,
    _amplifier_enable: Option<AMP>,
    chip: Es8311,
    descriptor: AudioDescriptor,
}

/// Static factory used by generated Board composition.
pub struct Es8311AudioCodecImplementation<I2C, I2S, AMP, DELAY>(
    PhantomData<I2C>,
    PhantomData<I2S>,
    PhantomData<AMP>,
    PhantomData<DELAY>,
);

impl<I2C, I2S, AMP, DELAY> PeripheralImplementation
    for Es8311AudioCodecImplementation<I2C, I2S, AMP, DELAY>
where
    I2C: I2c + 'static,
    I2S: PcmStream + 'static,
    AMP: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Es8311Bindings<I2C, I2S, AMP, DELAY>;
    type Config = Es8311Config;
    type Peripheral = Es8311AudioCodec<I2C, I2S, AMP>;
    type Error = Es8311InitError<I2C::Error, AMP::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let format = bindings.stream.format();
        if format.sample_rate_hz != 16_000 || format.channels != 2 || format.bits_per_sample != 16 {
            return Err(Es8311InitError::UnsupportedFormat);
        }
        let chip = Es8311::new(config.address).ok_or(Es8311InitError::InvalidAddress)?;
        if !config.enables.is_empty() {
            config
                .enables
                .supply_on(&mut bindings.control)
                .map_err(Es8311InitError::Helper)?;
            // M5Unified waits 10 ms for the audio rail before programming
            // the codec.
            bindings.delay.delay_ms(10);
        }
        chip.initialize_with_clock(&mut bindings.control, &mut bindings.delay, config.clock)
            .map_err(Es8311InitError::Control)?;
        config
            .enables
            .amplifier_on(&mut bindings.control)
            .map_err(Es8311InitError::Helper)?;
        if let Some(amplifier) = bindings.amplifier_enable.as_mut() {
            amplifier.set_high().map_err(Es8311InitError::Amplifier)?;
        }
        Ok(Es8311AudioCodec {
            control: bindings.control,
            stream: bindings.stream,
            _amplifier_enable: bindings.amplifier_enable,
            chip,
            descriptor: AudioDescriptor::new(
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
            ),
        })
    }
}

impl<I2C, I2S, AMP> AudioCodec for Es8311AudioCodec<I2C, I2S, AMP>
where
    I2C: I2c,
    I2S: PcmStream,
{
    type Error = Es8311Error<I2C::Error, I2S::Error>;

    fn descriptor(&self) -> AudioDescriptor {
        self.descriptor
    }

    fn set_output_volume(&mut self, volume: u8) -> Result<(), Self::Error> {
        self.chip
            .set_dac_volume(&mut self.control, volume)
            .map_err(Es8311Error::Control)
    }

    async fn write(&mut self, samples: &[i16]) -> Result<(), Self::Error> {
        self.stream
            .write(samples)
            .await
            .map_err(Es8311Error::Stream)
    }

    async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
        self.stream.read(samples).await.map_err(Es8311Error::Stream)
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
        digital::{ErrorType as DigitalErrorType, OutputPin},
        i2c::{ErrorType, I2c, Operation},
    };
    use portable_atomic::{AtomicBool, Ordering};

    use super::{Es8311AudioCodecImplementation, Es8311Bindings, Es8311Config};

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

    static AMPLIFIER_ENABLED: AtomicBool = AtomicBool::new(false);

    struct AmplifierEnable;
    impl DigitalErrorType for AmplifierEnable {
        type Error = Infallible;
    }
    impl OutputPin for AmplifierEnable {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            AMPLIFIER_ENABLED.store(false, Ordering::Relaxed);
            Ok(())
        }
        fn set_high(&mut self) -> Result<(), Self::Error> {
            AMPLIFIER_ENABLED.store(true, Ordering::Relaxed);
            Ok(())
        }
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
    fn initializes_registers_and_streams_pcm() {
        AMPLIFIER_ENABLED.store(false, Ordering::Relaxed);
        let bindings = Es8311Bindings::new(
            ControlBus::default(),
            Stream {
                written: std::vec::Vec::new(),
            },
            Some(AmplifierEnable),
            Delay,
        );
        let mut codec = block_on(Es8311AudioCodecImplementation::initialize(
            bindings,
            Es8311Config::new(0x18),
        ))
        .expect("initialize codec");
        assert!(AMPLIFIER_ENABLED.load(Ordering::Relaxed));
        assert_eq!(codec.descriptor().sample_rate_hz(), 16_000);
        block_on(codec.write(&[-1, 2])).expect("write PCM");
        let mut samples = [0_i16; 2];
        block_on(codec.read(&mut samples)).expect("read PCM");
        assert_eq!(samples, [0x1234, 0x1234]);
    }
}

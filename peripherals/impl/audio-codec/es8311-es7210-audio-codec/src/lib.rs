//! Full-duplex audio implementation for an ES8311 DAC and ES7210 microphone ADC.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_es7210::{Es7210, Es7210Error};
use barracuda_driver_es8311::{Es8311, Es8311Error};
use barracuda_peripheral::{
    PeripheralImplementation,
    audio::{AudioCodec, AudioDescriptor, PcmStream},
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c};

/// Board-owned addresses for the paired codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Es8311Es7210Config {
    output_address: u8,
    input_address: u8,
}

impl Es8311Es7210Config {
    /// Creates a paired-codec configuration.
    #[must_use]
    pub const fn new(output_address: u8, input_address: u8) -> Self {
        Self {
            output_address,
            input_address,
        }
    }
}

/// Move-only resources consumed by the paired audio implementation.
pub struct Es8311Es7210Bindings<I2C, I2S, POWER, AMP, DELAY> {
    control: I2C,
    stream: I2S,
    codec_power_enable: POWER,
    amplifier_enable: AMP,
    delay: DELAY,
}

impl<I2C, I2S, POWER, AMP, DELAY> Es8311Es7210Bindings<I2C, I2S, POWER, AMP, DELAY> {
    /// Combines control, PCM, power, amplifier, and delay resources.
    #[must_use]
    pub const fn new(
        control: I2C,
        stream: I2S,
        codec_power_enable: POWER,
        amplifier_enable: AMP,
        delay: DELAY,
    ) -> Self {
        Self {
            control,
            stream,
            codec_power_enable,
            amplifier_enable,
            delay,
        }
    }
}

/// Paired-codec initialization failure.
#[derive(Debug)]
pub enum Es8311Es7210InitError<ControlError, PowerError, AmplifierError> {
    /// ESP-VoCat audio requires 48 kHz stereo 16-bit PCM and 256-fs MCLK.
    UnsupportedFormat,
    /// One codec address is invalid.
    InvalidAddress,
    /// Codec power could not be enabled.
    Power(PowerError),
    /// ES8311 output-codec register programming failed.
    OutputControl(Es8311Error<ControlError>),
    /// ES7210 input-codec register programming failed.
    InputControl(Es7210Error<ControlError>),
    /// The external power amplifier could not be enabled.
    Amplifier(AmplifierError),
}

/// Paired-codec control or stream failure.
#[derive(Debug)]
pub enum Es8311Es7210Error<ControlError, StreamError> {
    /// Codec register access failed.
    OutputControl(Es8311Error<ControlError>),
    /// I2S transfer failed.
    Stream(StreamError),
}

/// Initialized ES8311 output, ES7210 input, and full-duplex PCM stream.
pub struct Es8311Es7210AudioCodec<I2C, I2S, POWER, AMP> {
    control: I2C,
    stream: I2S,
    _codec_power_enable: POWER,
    _amplifier_enable: AMP,
    output: Es8311,
    descriptor: AudioDescriptor,
}

/// Static factory used by generated Board composition.
pub struct Es8311Es7210AudioCodecImplementation<I2C, I2S, POWER, AMP, DELAY>(
    PhantomData<fn() -> (I2C, I2S, POWER, AMP, DELAY)>,
);

impl<I2C, I2S, POWER, AMP, DELAY> PeripheralImplementation
    for Es8311Es7210AudioCodecImplementation<I2C, I2S, POWER, AMP, DELAY>
where
    I2C: I2c + 'static,
    I2S: PcmStream + 'static,
    POWER: OutputPin + 'static,
    AMP: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Es8311Es7210Bindings<I2C, I2S, POWER, AMP, DELAY>;
    type Config = Es8311Es7210Config;
    type Peripheral = Es8311Es7210AudioCodec<I2C, I2S, POWER, AMP>;
    type Error = Es8311Es7210InitError<I2C::Error, POWER::Error, AMP::Error>;

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
            return Err(Es8311Es7210InitError::UnsupportedFormat);
        }
        let output =
            Es8311::new(config.output_address).ok_or(Es8311Es7210InitError::InvalidAddress)?;
        let input =
            Es7210::new(config.input_address).ok_or(Es8311Es7210InitError::InvalidAddress)?;
        bindings
            .codec_power_enable
            .set_high()
            .map_err(Es8311Es7210InitError::Power)?;
        bindings.delay.delay_ms(10);
        output
            .initialize(&mut bindings.control, &mut bindings.delay)
            .map_err(Es8311Es7210InitError::OutputControl)?;
        input
            .initialize(&mut bindings.control)
            .map_err(Es8311Es7210InitError::InputControl)?;
        bindings
            .amplifier_enable
            .set_high()
            .map_err(Es8311Es7210InitError::Amplifier)?;

        Ok(Es8311Es7210AudioCodec {
            control: bindings.control,
            stream: bindings.stream,
            _codec_power_enable: bindings.codec_power_enable,
            _amplifier_enable: bindings.amplifier_enable,
            output,
            descriptor: AudioDescriptor::new(
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
            ),
        })
    }
}

impl<I2C, I2S, POWER, AMP> AudioCodec for Es8311Es7210AudioCodec<I2C, I2S, POWER, AMP>
where
    I2C: I2c,
    I2S: PcmStream,
{
    type Error = Es8311Es7210Error<I2C::Error, I2S::Error>;

    fn descriptor(&self) -> AudioDescriptor {
        self.descriptor
    }

    fn set_output_volume(&mut self, volume: u8) -> Result<(), Self::Error> {
        self.output
            .set_dac_volume(&mut self.control, volume)
            .map_err(Es8311Es7210Error::OutputControl)
    }

    async fn write(&mut self, samples: &[i16]) -> Result<(), Self::Error> {
        self.stream
            .write(samples)
            .await
            .map_err(Es8311Es7210Error::Stream)
    }

    async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
        self.stream
            .read(samples)
            .await
            .map_err(Es8311Es7210Error::Stream)
    }
}

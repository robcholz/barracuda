//! Audio over a PDM microphone and an I2S amplifier, with no codec chip.
//!
//! M5Stack's smaller products record from an SPM1423 PDM microphone and play
//! through an NS4168 I2S class-D amplifier; neither has registers. The
//! Platform's `pdm-i2s-stream` binding carries both on one I2S controller,
//! one direction at a time, and this implementation adds what is left: the
//! amplifier's enable line, the PMIC output that powers the amplifier or the
//! microphone on the Core2, Tough, and StickC, and a software output volume.
//! The PMIC stays private to the implementation; the display that shares it
//! programs different outputs.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_axp192::{Axp192, Error as Axp192Error, LowGpio};
use barracuda_driver_axp2101::{Axp2101, Error as Axp2101Error, Ldo};
use barracuda_peripheral::{
    PeripheralImplementation,
    audio::{AudioCodec, AudioDescriptor, PcmStream},
};
use embedded_hal::{digital::OutputPin, i2c::I2c};

/// Seven-bit address of the AXP192 and AXP2101 on M5Stack's internal bus.
const PMIC_ADDRESS: u8 = 0x34;
/// M5Unified `_microphone_enabled_cb_stickc`: `setLDO0(2800)`.
const STICKC_MICROPHONE_MILLIVOLTS: u16 = 2800;
/// M5Unified `_speaker_enabled_cb_core2`: `setALDO3(3300)` on the AXP2101.
const CORE2_SPEAKER_MILLIVOLTS: u16 = 3300;
/// Samples scaled on the stack per chunk when the volume is below full.
const VOLUME_CHUNK: usize = 128;

/// Which PMIC output powers the audio parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PmicPower {
    /// No PMIC output is involved.
    None,
    /// The Core2 and Tough speaker amplifier: AXP192 GPIO2 floated high, or
    /// AXP2101 ALDO3 at 3.3 V on the Core2 v1.1.
    Core2Speaker,
    /// The StickC and StickC Plus microphone: AXP192 LDOio0 at 2.8 V.
    StickCMicrophone,
}

/// Audio power configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdmI2sAudioConfig {
    pmic_power: PmicPower,
}

impl PdmI2sAudioConfig {
    /// Creates a configuration powering the audio parts from `pmic_power`.
    #[must_use]
    pub const fn new(pmic_power: PmicPower) -> Self {
        Self { pmic_power }
    }
}

/// Move-only stream, amplifier enable, and PMIC bus.
pub struct PdmI2sAudioBindings<STREAM, AMP, PMIC> {
    stream: STREAM,
    amplifier_enable: Option<AMP>,
    pmic: Option<PMIC>,
}

impl<STREAM, AMP, PMIC> PdmI2sAudioBindings<STREAM, AMP, PMIC> {
    /// Combines the Platform stream with the optional amplifier enable and
    /// PMIC bus.
    #[must_use]
    pub const fn new(stream: STREAM, amplifier_enable: Option<AMP>, pmic: Option<PMIC>) -> Self {
        Self {
            stream,
            amplifier_enable,
            pmic,
        }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum PdmI2sAudioInitError<AmplifierError, BusError> {
    /// The stream is not 16-bit PCM.
    UnsupportedFormat,
    /// `pmic-power` names a PMIC output but the Board binds no PMIC bus.
    MissingPmic,
    /// The PMIC at 34h is neither the AXP192 nor the AXP2101 the layout needs.
    UnsupportedPmic,
    /// The amplifier enable line could not be driven.
    Amplifier(AmplifierError),
    /// The AXP192 rejected an operation.
    Axp192(Axp192Error<BusError>),
    /// The AXP2101 rejected an operation.
    Axp2101(Axp2101Error<BusError>),
}

/// Stream failure.
#[derive(Debug)]
pub enum PdmI2sAudioError<StreamError> {
    /// The PCM transfer failed, or the Board wired no line for its direction.
    Stream(StreamError),
}

/// Initialized codec-less audio.
pub struct PdmI2sAudioCodec<STREAM, AMP, PMIC> {
    stream: STREAM,
    _amplifier_enable: Option<AMP>,
    _pmic: Option<PMIC>,
    descriptor: AudioDescriptor,
    volume: u8,
}

/// Bindings carried only at the type level by the static factory.
type Parts<STREAM, AMP, PMIC> = PhantomData<fn() -> (STREAM, AMP, PMIC)>;

/// Static factory used by generated Board composition.
pub struct PdmI2sAudioCodecImplementation<STREAM, AMP, PMIC>(Parts<STREAM, AMP, PMIC>);

impl<STREAM, AMP, PMIC> PeripheralImplementation
    for PdmI2sAudioCodecImplementation<STREAM, AMP, PMIC>
where
    STREAM: PcmStream + 'static,
    AMP: OutputPin + 'static,
    PMIC: I2c + 'static,
{
    type Bindings = PdmI2sAudioBindings<STREAM, AMP, PMIC>;
    type Config = PdmI2sAudioConfig;
    type Peripheral = PdmI2sAudioCodec<STREAM, AMP, PMIC>;
    type Error = PdmI2sAudioInitError<AMP::Error, PMIC::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let format = bindings.stream.format();
        if format.bits_per_sample != 16 || format.channels == 0 {
            return Err(PdmI2sAudioInitError::UnsupportedFormat);
        }
        if config.pmic_power != PmicPower::None {
            let pmic = bindings
                .pmic
                .as_mut()
                .ok_or(PdmI2sAudioInitError::MissingPmic)?;
            power_audio(pmic, config.pmic_power)?;
        }
        if let Some(amplifier) = bindings.amplifier_enable.as_mut() {
            amplifier
                .set_high()
                .map_err(PdmI2sAudioInitError::Amplifier)?;
        }
        Ok(PdmI2sAudioCodec {
            stream: bindings.stream,
            _amplifier_enable: bindings.amplifier_enable,
            _pmic: bindings.pmic,
            descriptor: AudioDescriptor::new(
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
            ),
            volume: u8::MAX,
        })
    }
}

/// Switches on the PMIC output `power` names. Both PMICs answer at 34h; the
/// Core2 v1.0 has an AXP192 and the Core2 v1.1 an AXP2101, told apart by
/// their IC type register as M5Unified's `Power_Class::begin` does.
fn power_audio<I2C: I2c, A>(
    i2c: &mut I2C,
    power: PmicPower,
) -> Result<(), PdmI2sAudioInitError<A, I2C::Error>> {
    let axp192 = Axp192::new(i2c, PMIC_ADDRESS)
        .and_then(|mut pmic| pmic.is_present())
        .map_err(PdmI2sAudioInitError::Axp192)?;
    match (power, axp192) {
        (PmicPower::None, _) => Ok(()),
        (PmicPower::Core2Speaker, true) => Axp192::new(i2c, PMIC_ADDRESS)
            .and_then(|mut pmic| pmic.set_open_drain_level(LowGpio::Gpio2, true))
            .map_err(PdmI2sAudioInitError::Axp192),
        (PmicPower::StickCMicrophone, true) => Axp192::new(i2c, PMIC_ADDRESS)
            .and_then(|mut pmic| pmic.set_ldo_io0(Some(STICKC_MICROPHONE_MILLIVOLTS)))
            .map_err(PdmI2sAudioInitError::Axp192),
        (PmicPower::Core2Speaker, false) => {
            let mut pmic =
                Axp2101::new(i2c, PMIC_ADDRESS).map_err(PdmI2sAudioInitError::Axp2101)?;
            if !pmic.is_present().map_err(PdmI2sAudioInitError::Axp2101)? {
                return Err(PdmI2sAudioInitError::UnsupportedPmic);
            }
            pmic.set_millivolts(Ldo::Aldo3, CORE2_SPEAKER_MILLIVOLTS)
                .map_err(PdmI2sAudioInitError::Axp2101)?;
            pmic.set_enabled(Ldo::Aldo3, true)
                .map_err(PdmI2sAudioInitError::Axp2101)
        }
        (PmicPower::StickCMicrophone, false) => Err(PdmI2sAudioInitError::UnsupportedPmic),
    }
}

/// Scales one sample by a `0..=255` volume, 255 leaving it unchanged.
#[must_use]
pub const fn scale(sample: i16, volume: u8) -> i16 {
    ((sample as i32 * volume as i32) / u8::MAX as i32) as i16
}

impl<STREAM, AMP, PMIC> AudioCodec for PdmI2sAudioCodec<STREAM, AMP, PMIC>
where
    STREAM: PcmStream,
{
    type Error = PdmI2sAudioError<STREAM::Error>;

    fn descriptor(&self) -> AudioDescriptor {
        self.descriptor
    }

    /// The amplifiers have no gain register, so the volume scales samples.
    fn set_output_volume(&mut self, volume: u8) -> Result<(), Self::Error> {
        self.volume = volume;
        Ok(())
    }

    async fn write(&mut self, samples: &[i16]) -> Result<(), Self::Error> {
        if self.volume == u8::MAX {
            return self
                .stream
                .write(samples)
                .await
                .map_err(PdmI2sAudioError::Stream);
        }
        let mut scaled = [0_i16; VOLUME_CHUNK];
        for chunk in samples.chunks(VOLUME_CHUNK) {
            for (target, sample) in scaled.iter_mut().zip(chunk) {
                *target = scale(*sample, self.volume);
            }
            self.stream
                .write(&scaled[..chunk.len()])
                .await
                .map_err(PdmI2sAudioError::Stream)?;
        }
        Ok(())
    }

    async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
        self.stream
            .read(samples)
            .await
            .map_err(PdmI2sAudioError::Stream)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use barracuda_peripheral::audio::PcmFormat;
    use embedded_hal::i2c::{ErrorType, Operation};

    use super::*;

    #[derive(Debug)]
    struct StreamError;

    impl core::fmt::Display for StreamError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str("stream")
        }
    }

    impl core::error::Error for StreamError {}

    #[derive(Default)]
    struct Stream {
        written: Vec<i16>,
    }

    impl PcmStream for Stream {
        type Error = StreamError;

        fn format(&self) -> PcmFormat {
            PcmFormat {
                sample_rate_hz: 16_000,
                channels: 1,
                bits_per_sample: 16,
                master_clock_hz: None,
            }
        }

        async fn write(&mut self, samples: &[i16]) -> Result<(), Self::Error> {
            self.written.extend_from_slice(samples);
            Ok(())
        }

        async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
            samples.fill(7);
            Ok(())
        }
    }

    struct Pin(bool);

    impl embedded_hal::digital::ErrorType for Pin {
        type Error = Infallible;
    }

    impl OutputPin for Pin {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.0 = false;
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.0 = true;
            Ok(())
        }
    }

    /// Register file at 34h answering with the given IC type.
    struct Pmic {
        registers: [u8; 0x100],
    }

    impl Pmic {
        fn new(ic_type: u8) -> Self {
            let mut registers = [0; 0x100];
            registers[0x03] = ic_type;
            Self { registers }
        }
    }

    impl ErrorType for Pmic {
        type Error = Infallible;
    }

    impl I2c for Pmic {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, PMIC_ADDRESS);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = *reg,
                    Operation::Write([reg, value]) => self.registers[usize::from(*reg)] = *value,
                    Operation::Read([value]) => *value = self.registers[usize::from(register)],
                    _ => {}
                }
            }
            Ok(())
        }
    }

    fn start(
        pmic: Option<Pmic>,
        power: PmicPower,
    ) -> Result<PdmI2sAudioCodec<Stream, Pin, Pmic>, PdmI2sAudioInitError<Infallible, Infallible>>
    {
        embassy_futures::block_on(
            PdmI2sAudioCodecImplementation::<Stream, Pin, Pmic>::initialize(
                PdmI2sAudioBindings::new(Stream::default(), Some(Pin(false)), pmic),
                PdmI2sAudioConfig::new(power),
            ),
        )
    }

    #[test]
    fn enables_amplifier_and_scales_volume() {
        let mut audio = start(None, PmicPower::None).unwrap();
        assert!(audio._amplifier_enable.as_ref().is_some_and(|pin| pin.0));
        assert_eq!(audio.descriptor().channels(), 1);
        embassy_futures::block_on(audio.write(&[1000, -1000])).unwrap();
        audio.set_output_volume(51).unwrap();
        embassy_futures::block_on(audio.write(&[1000, -1000])).unwrap();
        assert_eq!(audio.stream.written, [1000, -1000, 200, -200]);
        let mut samples = [0; 3];
        embassy_futures::block_on(audio.read(&mut samples)).unwrap();
        assert_eq!(samples, [7; 3]);
    }

    #[test]
    fn powers_the_core2_speaker_from_either_pmic() {
        let audio = start(Some(Pmic::new(0x03)), PmicPower::Core2Speaker).unwrap();
        // AXP192: GPIO2 (REG93H) floats high.
        assert_eq!(audio._pmic.as_ref().unwrap().registers[0x93], 0x06);
        let audio = start(Some(Pmic::new(0x4a)), PmicPower::Core2Speaker).unwrap();
        // AXP2101: ALDO3 (REG94H) at 3.3 V, enabled in REG90H bit 2.
        let registers = audio._pmic.as_ref().unwrap().registers;
        assert_eq!(registers[0x94], 28);
        assert_eq!(registers[0x90] & 0b100, 0b100);
    }

    #[test]
    fn powers_the_stickc_microphone_and_requires_a_pmic() {
        let audio = start(Some(Pmic::new(0x03)), PmicPower::StickCMicrophone).unwrap();
        let registers = audio._pmic.as_ref().unwrap().registers;
        assert_eq!(registers[0x91] >> 4, 10);
        assert_eq!(registers[0x90], 0x02);
        assert!(matches!(
            start(None, PmicPower::StickCMicrophone),
            Err(PdmI2sAudioInitError::MissingPmic)
        ));
        assert!(matches!(
            start(Some(Pmic::new(0x4a)), PmicPower::StickCMicrophone),
            Err(PdmI2sAudioInitError::UnsupportedPmic)
        ));
    }
}

//! Full-duplex audio for an AW88298 speaker amplifier and an ES7210
//! microphone ADC on one I2S stream.
//!
//! On the M5Stack CoreS3 the AW88298 takes the I2S output and the ES7210
//! feeds the I2S input; both answer on the internal bus beside the AXP2101
//! PMIC, which supplies them, and the AW9523B expander, which holds the
//! amplifier in reset. This implementation powers both, releases the reset,
//! programs the ES7210 and starts the AW88298 at the stream's sample rate.
//! The PMIC and the expander stay private: the display that shares them
//! programs other outputs (the backlight rail and the LCD reset).

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_aw9523::{Aw9523, Error as Aw9523Error};
use barracuda_driver_aw88298::{Aw88298, Aw88298Error};
use barracuda_driver_axp2101::{Axp2101, Error as Axp2101Error, Ldo};
use barracuda_driver_es7210::{Es7210, Es7210Error};
use barracuda_peripheral::{
    PeripheralImplementation,
    audio::{AudioCodec, AudioDescriptor, PcmStream},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

/// AXP2101 address on M5Stack's internal bus.
const PMIC_ADDRESS: u8 = 0x34;
/// AW9523B address with AD0 and AD1 low (M5GFX `aw9523_i2c_addr`).
const EXPANDER_ADDRESS: u8 = 0x58;
/// AW9523B P0_2, the AW88298 reset (M5Unified `_core_s3_aw9523_bit(0x02,
/// 0b00000100, true)`).
const AMPLIFIER_RESET_PIN: u8 = 2;
/// M5Unified `Power_Class::begin` CoreS3: ALDO1 1.8 V for the AW88298,
/// ALDO2 3.3 V for the ES7210.
const AMPLIFIER_SUPPLY_MILLIVOLTS: u16 = 1800;
const MICROPHONE_SUPPLY_MILLIVOLTS: u16 = 3300;
/// Time for the supplies and the released reset to settle.
const SETTLE_MS: u32 = 10;
/// Samples scaled on the stack per chunk when the volume is below full.
const VOLUME_CHUNK: usize = 128;

/// Which helper chips power and reset the audio parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerLayout {
    /// CoreS3, CoreS3 SE, StackChan: AXP2101 ALDO1/ALDO2 and AW9523B P0_2.
    CoreS3,
}

/// Board-owned addresses and power layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aw88298Es7210Config {
    amplifier_address: u8,
    input_address: u8,
    layout: PowerLayout,
}

impl Aw88298Es7210Config {
    /// Creates a configuration.
    #[must_use]
    pub const fn new(amplifier_address: u8, input_address: u8, layout: PowerLayout) -> Self {
        Self {
            amplifier_address,
            input_address,
            layout,
        }
    }
}

/// Move-only control bus, stream, and delay.
pub struct Aw88298Es7210Bindings<I2C, I2S, DELAY> {
    control: I2C,
    stream: I2S,
    delay: DELAY,
}

impl<I2C, I2S, DELAY> Aw88298Es7210Bindings<I2C, I2S, DELAY> {
    /// Combines the internal bus, the full-duplex stream, and a delay.
    #[must_use]
    pub const fn new(control: I2C, stream: I2S, delay: DELAY) -> Self {
        Self {
            control,
            stream,
            delay,
        }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum Aw88298Es7210InitError<BusError> {
    /// The ES7210 needs 48 kHz, 16-bit stereo PCM with a 256-fs MCLK.
    UnsupportedFormat,
    /// One device address is invalid.
    InvalidAddress,
    /// The AXP2101 rejected an operation.
    Pmic(Axp2101Error<BusError>),
    /// The AW9523B rejected an operation.
    Expander(Aw9523Error<BusError>),
    /// AW88298 register programming failed.
    Amplifier(Aw88298Error<BusError>),
    /// ES7210 register programming failed.
    Microphone(Es7210Error<BusError>),
}

/// Stream failure.
#[derive(Debug)]
pub enum Aw88298Es7210Error<StreamError> {
    /// The I2S transfer failed.
    Stream(StreamError),
}

/// Initialized amplifier, microphone ADC, and stream.
pub struct Aw88298Es7210AudioCodec<I2C, I2S> {
    _control: I2C,
    stream: I2S,
    descriptor: AudioDescriptor,
    volume: u8,
}

/// Bindings carried only at the type level by the static factory.
type Parts<I2C, I2S, DELAY> = PhantomData<fn() -> (I2C, I2S, DELAY)>;

/// Static factory used by generated Board composition.
pub struct Aw88298Es7210AudioCodecImplementation<I2C, I2S, DELAY>(Parts<I2C, I2S, DELAY>);

impl<I2C, I2S, DELAY> PeripheralImplementation
    for Aw88298Es7210AudioCodecImplementation<I2C, I2S, DELAY>
where
    I2C: I2c + 'static,
    I2S: PcmStream + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Aw88298Es7210Bindings<I2C, I2S, DELAY>;
    type Config = Aw88298Es7210Config;
    type Peripheral = Aw88298Es7210AudioCodec<I2C, I2S>;
    type Error = Aw88298Es7210InitError<I2C::Error>;

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
            return Err(Aw88298Es7210InitError::UnsupportedFormat);
        }
        let amplifier =
            Aw88298::new(config.amplifier_address).ok_or(Aw88298Es7210InitError::InvalidAddress)?;
        let microphone =
            Es7210::new(config.input_address).ok_or(Aw88298Es7210InitError::InvalidAddress)?;
        match config.layout {
            PowerLayout::CoreS3 => power_cores3(&mut bindings.control)?,
        }
        bindings.delay.delay_ms(SETTLE_MS);
        microphone
            .initialize(&mut bindings.control)
            .map_err(Aw88298Es7210InitError::Microphone)?;
        amplifier
            .start(&mut bindings.control, format.sample_rate_hz)
            .map_err(Aw88298Es7210InitError::Amplifier)?;
        Ok(Aw88298Es7210AudioCodec {
            _control: bindings.control,
            stream: bindings.stream,
            descriptor: AudioDescriptor::new(
                format.sample_rate_hz,
                format.channels,
                format.bits_per_sample,
            ),
            volume: u8::MAX,
        })
    }
}

/// Supplies both parts and releases the amplifier's reset.
fn power_cores3<I2C: I2c>(i2c: &mut I2C) -> Result<(), Aw88298Es7210InitError<I2C::Error>> {
    let mut pmic = Axp2101::new(i2c, PMIC_ADDRESS).map_err(Aw88298Es7210InitError::Pmic)?;
    for (ldo, millivolts) in [
        (Ldo::Aldo1, AMPLIFIER_SUPPLY_MILLIVOLTS),
        (Ldo::Aldo2, MICROPHONE_SUPPLY_MILLIVOLTS),
    ] {
        pmic.set_millivolts(ldo, millivolts)
            .map_err(Aw88298Es7210InitError::Pmic)?;
        pmic.set_enabled(ldo, true)
            .map_err(Aw88298Es7210InitError::Pmic)?;
    }
    let mut expander =
        Aw9523::new(i2c, EXPANDER_ADDRESS).map_err(Aw88298Es7210InitError::Expander)?;
    // M5GFX's CoreS3 setup drives port 0 push-pull.
    expander
        .set_port0_push_pull(true)
        .map_err(Aw88298Es7210InitError::Expander)?;
    expander
        .configure_output(AMPLIFIER_RESET_PIN, true)
        .map_err(Aw88298Es7210InitError::Expander)
}

/// Scales one sample by a `0..=255` volume, 255 leaving it unchanged.
#[must_use]
pub const fn scale(sample: i16, volume: u8) -> i16 {
    ((sample as i32 * volume as i32) / u8::MAX as i32) as i16
}

impl<I2C, I2S> AudioCodec for Aw88298Es7210AudioCodec<I2C, I2S>
where
    I2S: PcmStream,
{
    type Error = Aw88298Es7210Error<I2S::Error>;

    fn descriptor(&self) -> AudioDescriptor {
        self.descriptor
    }

    /// The amplifier runs at full volume; the volume scales samples.
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
                .map_err(Aw88298Es7210Error::Stream);
        }
        let mut scaled = [0_i16; VOLUME_CHUNK];
        for chunk in samples.chunks(VOLUME_CHUNK) {
            for (target, sample) in scaled.iter_mut().zip(chunk) {
                *target = scale(*sample, self.volume);
            }
            self.stream
                .write(&scaled[..chunk.len()])
                .await
                .map_err(Aw88298Es7210Error::Stream)?;
        }
        Ok(())
    }

    async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
        self.stream
            .read(samples)
            .await
            .map_err(Aw88298Es7210Error::Stream)
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

    /// Register files of every device on the bus, by address.
    #[derive(Default)]
    struct Bus {
        registers: std::collections::BTreeMap<(u8, u8), u8>,
        writes: Vec<(u8, Vec<u8>)>,
    }

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Infallible> {
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = *reg,
                    Operation::Write(bytes) => {
                        if let [reg, value] = **bytes {
                            self.registers.insert((address, reg), value);
                        }
                        self.writes.push((address, bytes.to_vec()));
                    }
                    Operation::Read([value]) => {
                        *value = self
                            .registers
                            .get(&(address, register))
                            .copied()
                            .unwrap_or(0);
                    }
                    Operation::Read(_) => {}
                }
            }
            Ok(())
        }
    }

    struct Stream(PcmFormat);

    #[derive(Debug)]
    struct StreamError;

    impl core::fmt::Display for StreamError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str("stream")
        }
    }

    impl core::error::Error for StreamError {}

    impl PcmStream for Stream {
        type Error = StreamError;

        fn format(&self) -> PcmFormat {
            self.0
        }

        async fn write(&mut self, _: &[i16]) -> Result<(), StreamError> {
            Ok(())
        }

        async fn read(&mut self, samples: &mut [i16]) -> Result<(), StreamError> {
            samples.fill(1);
            Ok(())
        }
    }

    struct Delay;

    impl DelayNs for Delay {
        fn delay_ns(&mut self, _: u32) {}
    }

    const FORMAT: PcmFormat = PcmFormat {
        sample_rate_hz: 48_000,
        channels: 2,
        bits_per_sample: 16,
        master_clock_hz: Some(12_288_000),
    };

    fn start(
        format: PcmFormat,
    ) -> Result<Aw88298Es7210AudioCodec<Bus, Stream>, Aw88298Es7210InitError<Infallible>> {
        embassy_futures::block_on(
            Aw88298Es7210AudioCodecImplementation::<Bus, Stream, Delay>::initialize(
                Aw88298Es7210Bindings::new(Bus::default(), Stream(format), Delay),
                Aw88298Es7210Config::new(0x36, 0x40, PowerLayout::CoreS3),
            ),
        )
    }

    #[test]
    fn powers_releases_and_starts_both_parts() {
        let codec = start(FORMAT).unwrap();
        let bus = &codec._control;
        // AXP2101 ALDO1 (92h) 1.8 V and ALDO2 (93h) 3.3 V, enabled in 90h.
        assert_eq!(bus.registers.get(&(0x34, 0x92)), Some(&13));
        assert_eq!(bus.registers.get(&(0x34, 0x93)), Some(&28));
        assert_eq!(
            bus.registers.get(&(0x34, 0x90)).map(|value| value & 0b11),
            Some(0b11)
        );
        // AW9523B P0_2 latched high before it becomes an output.
        assert_eq!(
            bus.registers.get(&(0x58, 0x02)).map(|value| value & 0b100),
            Some(0b100)
        );
        // The ES7210 is programmed before the AW88298 starts at 48 kHz.
        let es7210 = bus.writes.iter().position(|(address, _)| *address == 0x40);
        let aw88298 = bus.writes.iter().position(|(address, _)| *address == 0x36);
        assert!(es7210 < aw88298);
        assert!(
            bus.writes
                .iter()
                .any(|(address, bytes)| *address == 0x36 && bytes == &[0x06, 0x14, 0xc8])
        );
    }

    #[test]
    fn rejects_other_formats_and_streams_pcm() {
        let other = PcmFormat {
            sample_rate_hz: 16_000,
            ..FORMAT
        };
        assert!(matches!(
            start(other),
            Err(Aw88298Es7210InitError::UnsupportedFormat)
        ));
        let mut codec = start(FORMAT).unwrap();
        let mut samples = [0; 4];
        embassy_futures::block_on(codec.read(&mut samples)).unwrap();
        assert_eq!(samples, [1; 4]);
        assert_eq!(scale(1000, 51), 200);
    }
}

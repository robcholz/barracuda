//! WS2812/SK6812 LED-strip implementation over a pulse-train output.
//!
//! Each bit is one symbol at 10 MHz ticks: a zero is 0.3 µs high then
//! 0.9 µs low, a one 0.9 µs high then 0.3 µs low, and a frame ends with
//! 280 µs low, the timings of ESP-IDF's `led_strip` RMT encoder
//! (`led_strip_rmt_encoder.c`), which every WS2812B and SK6812 revision
//! accepts. Pixels go out in the chain's color order, most significant bit
//! first.
//!
//! The symbol buffer for the whole chain is allocated once at start, so a
//! write allocates nothing and is sent in one blocking transfer.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use core::marker::PhantomData;

use barracuda_board_hal::{PulseLevel, PulseOutput, PulseOutputConfig};
use barracuda_peripheral::{
    PeripheralImplementation,
    led_strip::{LedStrip, Rgb8},
};
use embedded_hal::digital::OutputPin;

/// Symbol tick frequency.
const TICK_HZ: u32 = 10_000_000;
/// Short and long halves of a bit, in 100 ns ticks.
const SHORT_TICKS: u16 = 3;
const LONG_TICKS: u16 = 9;
/// Half of the 280 µs reset gap that latches a frame.
const RESET_HALF_TICKS: u16 = 1_400;
const BITS_PER_PIXEL: usize = 24;

/// Component order shifted into the one-wire LED chain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorOrder {
    /// Green, red, then blue; used by WS2812 and most SK6812 RGB parts.
    #[default]
    Grb,
    /// Red, green, then blue.
    Rgb,
}

/// Board-owned LED-strip configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RmtLedStripConfig {
    led_count: usize,
    color_order: ColorOrder,
}

impl RmtLedStripConfig {
    /// Creates configuration for one fixed Board LED chain.
    #[must_use]
    pub const fn new(led_count: usize, color_order: ColorOrder) -> Self {
        Self {
            led_count,
            color_order,
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct RmtLedStripBindings<OUT, POWER> {
    data: OUT,
    power_enable: Option<POWER>,
}

impl<OUT, POWER> RmtLedStripBindings<OUT, POWER> {
    /// Wraps the chain's data output and its optional supply switch.
    #[must_use]
    pub const fn new(data: OUT, power_enable: Option<POWER>) -> Self {
        Self { data, power_enable }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum RmtLedStripInitError<OutputError, PinError> {
    /// A strip must contain at least one LED.
    EmptyStrip,
    /// The output rejected the WS2812 timing.
    Output(OutputError),
    /// The supply switch could not be turned on.
    PowerEnable(PinError),
}

/// Output failure.
#[derive(Debug)]
pub enum RmtLedStripError<OutputError> {
    /// More pixels were supplied than physically configured.
    TooManyPixels,
    /// The pulse-train transfer failed.
    Output(OutputError),
}

/// Initialized WS2812-compatible LED strip.
pub struct RmtLedStrip<OUT: PulseOutput, POWER> {
    output: OUT,
    _power_enable: Option<POWER>,
    color_order: ColorOrder,
    led_count: usize,
    symbols: Vec<OUT::Symbol>,
}

/// Static factory used by generated Board composition.
pub struct RmtLedStripImplementation<OUT, POWER>(PhantomData<fn() -> (OUT, POWER)>);

impl<OUT, POWER> PeripheralImplementation for RmtLedStripImplementation<OUT, POWER>
where
    OUT: PulseOutput + 'static,
    POWER: OutputPin + 'static,
{
    type Bindings = RmtLedStripBindings<OUT, POWER>;
    type Config = RmtLedStripConfig;
    type Peripheral = RmtLedStrip<OUT, POWER>;
    type Error = RmtLedStripInitError<OUT::Error, POWER::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        RmtLedStrip::new(bindings, config)
    }
}

impl<OUT, POWER> RmtLedStrip<OUT, POWER>
where
    OUT: PulseOutput,
    POWER: OutputPin,
{
    fn new(
        bindings: RmtLedStripBindings<OUT, POWER>,
        config: RmtLedStripConfig,
    ) -> Result<Self, RmtLedStripInitError<OUT::Error, POWER::Error>> {
        if config.led_count == 0 {
            return Err(RmtLedStripInitError::EmptyStrip);
        }
        let mut output = bindings.data;
        output
            .configure(PulseOutputConfig {
                tick_hz: TICK_HZ,
                idle_high: false,
                carrier: None,
            })
            .map_err(RmtLedStripInitError::Output)?;
        let mut power_enable = bindings.power_enable;
        if let Some(power) = power_enable.as_mut() {
            power
                .set_high()
                .map_err(RmtLedStripInitError::PowerEnable)?;
        }
        // Every bit, the reset gap, and the end marker.
        let length = config
            .led_count
            .saturating_mul(BITS_PER_PIXEL)
            .saturating_add(2);
        let mut symbols = Vec::with_capacity(length);
        symbols.resize(length, OUT::end_marker());
        Ok(Self {
            output,
            _power_enable: power_enable,
            color_order: config.color_order,
            led_count: config.led_count,
            symbols,
        })
    }
}

impl<OUT: PulseOutput, POWER> RmtLedStrip<OUT, POWER> {
    /// Returns the pulse output to its owner.
    #[must_use]
    pub fn into_output(self) -> OUT {
        self.output
    }
}

fn bit_symbol<OUT: PulseOutput>(one: bool) -> OUT::Symbol {
    let (high, low) = if one {
        (LONG_TICKS, SHORT_TICKS)
    } else {
        (SHORT_TICKS, LONG_TICKS)
    };
    OUT::symbol(PulseLevel::new(true, high), PulseLevel::new(false, low))
}

impl<OUT, POWER> LedStrip for RmtLedStrip<OUT, POWER>
where
    OUT: PulseOutput,
{
    type Error = RmtLedStripError<OUT::Error>;

    fn len(&self) -> usize {
        self.led_count
    }

    fn write(&mut self, pixels: &[Rgb8]) -> Result<(), Self::Error> {
        if pixels.len() > self.led_count {
            return Err(RmtLedStripError::TooManyPixels);
        }
        let zero = bit_symbol::<OUT>(false);
        let one = bit_symbol::<OUT>(true);
        let blank = Rgb8::default();
        let mut slots = self.symbols.iter_mut();
        for index in 0..self.led_count {
            let pixel = pixels.get(index).unwrap_or(&blank);
            let bytes = match self.color_order {
                ColorOrder::Grb => [pixel.green, pixel.red, pixel.blue],
                ColorOrder::Rgb => [pixel.red, pixel.green, pixel.blue],
            };
            for byte in bytes {
                for bit in (0..8).rev() {
                    if let Some(slot) = slots.next() {
                        *slot = if (byte >> bit) & 1 == 1 { one } else { zero };
                    }
                }
            }
        }
        if let Some(slot) = slots.next() {
            *slot = OUT::symbol(
                PulseLevel::new(false, RESET_HALF_TICKS),
                PulseLevel::new(false, RESET_HALF_TICKS),
            );
        }
        if let Some(slot) = slots.next() {
            *slot = OUT::end_marker();
        }
        self.output
            .transmit_blocking(&self.symbols)
            .map_err(RmtLedStripError::Output)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use barracuda_board_hal::{PulseLevel, PulseOutput, PulseOutputConfig};
    use barracuda_peripheral::led_strip::{LedStrip, Rgb8};
    use embedded_hal::digital::{ErrorType, OutputPin};

    use super::{ColorOrder, RmtLedStrip, RmtLedStripBindings, RmtLedStripConfig};

    #[derive(Default)]
    struct RecordingOutput {
        config: Option<PulseOutputConfig>,
        sent: Vec<(PulseLevel, PulseLevel)>,
    }

    impl PulseOutput for RecordingOutput {
        type Symbol = (PulseLevel, PulseLevel);
        type Error = Infallible;

        fn symbol(first: PulseLevel, second: PulseLevel) -> Self::Symbol {
            (first, second)
        }

        fn end_marker() -> Self::Symbol {
            (PulseLevel::new(false, 0), PulseLevel::new(false, 0))
        }

        fn configure(&mut self, config: PulseOutputConfig) -> Result<(), Self::Error> {
            self.config = Some(config);
            Ok(())
        }

        fn transmit_blocking(&mut self, symbols: &[Self::Symbol]) -> Result<(), Self::Error> {
            self.sent = symbols.to_vec();
            Ok(())
        }

        async fn transmit(&mut self, symbols: &[Self::Symbol]) -> Result<(), Self::Error> {
            self.transmit_blocking(symbols)
        }
    }

    #[derive(Default)]
    struct Switch {
        high: bool,
    }

    impl ErrorType for Switch {
        type Error = Infallible;
    }

    impl OutputPin for Switch {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.high = false;
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.high = true;
            Ok(())
        }
    }

    fn strip(count: usize) -> RmtLedStrip<RecordingOutput, Switch> {
        RmtLedStrip::new(
            RmtLedStripBindings::new(RecordingOutput::default(), Some(Switch::default())),
            RmtLedStripConfig::new(count, ColorOrder::Grb),
        )
        .expect("start strip")
    }

    #[test]
    fn starts_at_ten_megahertz_with_the_supply_on() {
        let strip = strip(1);
        assert!(strip._power_enable.as_ref().is_some_and(|power| power.high));
        let config = strip.output.config.expect("configured");
        assert_eq!(config.tick_hz, 10_000_000);
        assert!(!config.idle_high && config.carrier.is_none());
    }

    #[test]
    fn encodes_grb_msb_first_and_blanks_trailing_pixels() {
        let mut strip = strip(2);
        strip.write(&[Rgb8::new(0x80, 0x01, 0)]).expect("write");
        let sent = &strip.output.sent;
        assert_eq!(sent.len(), 2 * 24 + 2);
        let one = (PulseLevel::new(true, 9), PulseLevel::new(false, 3));
        let zero = (PulseLevel::new(true, 3), PulseLevel::new(false, 9));
        // Green 0x01: only its last bit is set.
        assert_eq!(sent[6], zero);
        assert_eq!(sent[7], one);
        // Red 0x80: its first bit.
        assert_eq!(sent[8], one);
        assert_eq!(sent[9], zero);
        // The second pixel is off.
        assert!(sent[24..48].iter().all(|symbol| *symbol == zero));
        assert_eq!(sent[48].0, PulseLevel::new(false, 1_400));
        assert_eq!(sent[49].0.ticks, 0);
    }

    #[test]
    fn rejects_more_pixels_than_the_chain_has() {
        let mut strip = strip(1);
        assert!(strip.write(&[Rgb8::default(); 2]).is_err());
        assert!(
            RmtLedStrip::<RecordingOutput, Switch>::new(
                RmtLedStripBindings::new(RecordingOutput::default(), None),
                RmtLedStripConfig::new(0, ColorOrder::Grb),
            )
            .is_err()
        );
    }
}

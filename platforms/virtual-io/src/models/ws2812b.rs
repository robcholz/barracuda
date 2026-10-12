//! Worldsemi WS2812B intelligent LED chain driven from an SPI MOSI line.
//!
//! Source: Worldsemi "WS2812B Intelligent control LED integrated light
//! source" datasheet (undated; read as hosted at
//! cdn-shop.adafruit.com/datasheets/WS2812B.pdf): the "Data transfer time"
//! table, the sequence chart, and "Composition of 24bit data".
//!
//! The model decodes the MOSI bit stream at the bus clock into high and low
//! pulse widths and checks each data bit against the table: a 0 code is
//! T0H 0.4 µs and T0L 0.85 µs, a 1 code T1H 0.8 µs and T1L 0.45 µs, each
//! ±150 ns; a low time above 50 µs (RES) latches the data. Frames are 24 bits
//! in G, R, B order, most significant bit first; the first frame after a
//! reset belongs to the first LED. The line is assumed low between transfers
//! (an idle MOSI), so a gap counts toward RES.
//!
//! Backdoor layout: three bytes per LED in wire order (G, R, B) of the last
//! latched data.

use std::time::Duration;

use crate::{
    device::{DeviceContext, RegisterRangeError},
    spi::SpiDevice,
};

const PICOS_PER_NANO: u64 = 1_000;
/// Data transfer time table: T0H 0.4 µs.
const T0H_NS: u64 = 400;
/// Data transfer time table: T1H 0.8 µs.
const T1H_NS: u64 = 800;
/// Data transfer time table: T0L 0.85 µs.
const T0L_NS: u64 = 850;
/// Data transfer time table: T1L 0.45 µs.
const T1L_NS: u64 = 450;
/// Data transfer time table: tolerance ±150 ns.
const TOLERANCE_NS: u64 = 150;
/// Data transfer time table: RES, low voltage time above 50 µs.
const RESET_NS: u64 = 50_000;

/// Rule identifiers reported by this model.
pub mod rules {
    /// Data transfer time table: a bit's high and low times match a code.
    pub const BIT_TIMING: &str = "WS2812B datasheet data transfer time T0H/T0L/T1H/T1L";
    /// Composition of 24bit data: a latch follows whole 24-bit frames.
    pub const FRAME: &str = "WS2812B datasheet composition of 24bit data";
}

/// Behavioural model of a chain of `leds` WS2812B pixels.
#[derive(Clone, Debug)]
pub struct Ws2812bModel {
    leds: usize,
    displayed: Vec<u8>,
    bits: Vec<bool>,
    high: bool,
    run_ps: u64,
    pending_high_ps: Option<u64>,
    latched: bool,
    last_end: Option<Duration>,
}

impl Ws2812bModel {
    /// Creates a chain of `leds` pixels, all dark.
    #[must_use]
    pub fn new(leds: usize) -> Self {
        Self {
            leds,
            displayed: vec![0; leds * 3],
            bits: Vec::new(),
            high: false,
            run_ps: 0,
            pending_high_ps: None,
            latched: true,
            last_end: None,
        }
    }

    fn within(actual_ps: u64, nominal_ns: u64) -> bool {
        let nominal = nominal_ns * PICOS_PER_NANO;
        let tolerance = TOLERANCE_NS * PICOS_PER_NANO;
        actual_ps + tolerance >= nominal && actual_ps <= nominal + tolerance
    }

    /// Classifies one data bit from its high time and, unless a reset ends
    /// it, its low time.
    fn symbol(&mut self, context: &mut DeviceContext<'_>, high_ps: u64, low_ps: Option<u64>) {
        let zero =
            Self::within(high_ps, T0H_NS) && low_ps.is_none_or(|low| Self::within(low, T0L_NS));
        let one =
            Self::within(high_ps, T1H_NS) && low_ps.is_none_or(|low| Self::within(low, T1L_NS));
        if zero == one {
            context.violation(
                rules::BIT_TIMING,
                format!(
                    "bit {} high {} ns, low {} matches no code",
                    self.bits.len(),
                    high_ps / PICOS_PER_NANO,
                    low_ps.map_or_else(
                        || String::from("until reset"),
                        |low| format!("{} ns", low / PICOS_PER_NANO)
                    ),
                ),
            );
        }
        self.bits.push(one && !zero);
        self.latched = false;
    }

    fn latch(&mut self, context: &mut DeviceContext<'_>) {
        if let Some(high) = self.pending_high_ps.take() {
            self.symbol(context, high, None);
        }
        if self.latched {
            return;
        }
        self.latched = true;
        if !self.bits.len().is_multiple_of(24) {
            context.violation(
                rules::FRAME,
                format!("{} bits latched; frames are 24 bits", self.bits.len()),
            );
        }
        let frames = self.bits.len() / 24;
        for (index, frame) in self
            .bits
            .as_chunks::<24>()
            .0
            .iter()
            .take(self.leds)
            .enumerate()
        {
            for (byte, bits) in frame.as_chunks::<8>().0.iter().enumerate() {
                self.displayed[index * 3 + byte] = bits
                    .iter()
                    .fold(0, |value, bit| (value << 1) | u8::from(*bit));
            }
        }
        context.note(format!(
            "latched {} of {} LEDs",
            frames.min(self.leds),
            self.leds
        ));
        self.bits.clear();
    }

    fn end_run(&mut self, context: &mut DeviceContext<'_>) {
        if self.high {
            self.pending_high_ps = Some(self.run_ps);
        } else if let Some(high) = self.pending_high_ps.take() {
            self.symbol(context, high, Some(self.run_ps));
        }
        self.run_ps = 0;
    }

    fn extend_low(&mut self, context: &mut DeviceContext<'_>, picos: u64) {
        self.run_ps += picos;
        if self.run_ps > RESET_NS * PICOS_PER_NANO {
            self.latch(context);
        }
    }
}

impl SpiDevice for Ws2812bModel {
    fn model(&self) -> &'static str {
        "ws2812b"
    }

    fn transfer(
        &mut self,
        context: &mut DeviceContext<'_>,
        frequency_hz: u32,
        write: &[u8],
        _read: &mut [u8],
    ) {
        let now = context.now();
        if let Some(end) = self.last_end {
            let gap = u64::try_from(now.saturating_sub(end).as_nanos()).unwrap_or(u64::MAX);
            if self.high {
                self.end_run(context);
                self.high = false;
            }
            self.extend_low(context, gap.saturating_mul(PICOS_PER_NANO));
        }
        let bit_ps = 1_000_000_000_000 / u64::from(frequency_hz.max(1));
        for byte in write {
            for shift in (0..8).rev() {
                let high = byte & (1 << shift) != 0;
                if high != self.high {
                    self.end_run(context);
                    self.high = high;
                }
                if high {
                    self.run_ps += bit_ps;
                } else {
                    self.extend_low(context, bit_ps);
                }
            }
        }
        let bits = u64::try_from(write.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(8);
        self.last_end =
            Some(now + Duration::from_nanos(bits.saturating_mul(bit_ps) / PICOS_PER_NANO));
    }

    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
        let source = crate::device::register_range(&self.displayed, offset, buffer.len())?;
        buffer.copy_from_slice(source);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::spi::SpiBus as _;

    use super::*;
    use crate::{Clock, VirtualHardware};

    /// One WS2812B bit as three SPI bits at 2.4 MHz (416.7 ns each).
    fn encode(bytes: &[u8]) -> Vec<u8> {
        let mut bits = Vec::new();
        for byte in bytes {
            for shift in (0..8).rev() {
                let one = byte & (1 << shift) != 0;
                bits.extend_from_slice(if one {
                    &[true, true, false]
                } else {
                    &[true, false, false]
                });
            }
        }
        bits.chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0, |v, (i, b)| v | (u8::from(*b) << (7 - i)))
            })
            .collect()
    }

    fn setup(frequency_hz: u32) -> (VirtualHardware, crate::VirtualSpiBus) {
        let hardware = VirtualHardware::new(&[], &[], Clock::manual()).with_spi(&["SPI2"]);
        hardware
            .attach_spi("SPI2", Box::new(Ws2812bModel::new(2)))
            .expect("attach");
        let bus = hardware.spi_bus("SPI2", frequency_hz).expect("bus");
        (hardware, bus)
    }

    #[test]
    fn frames_latch_after_a_reset_low_time() {
        let (hardware, mut bus) = setup(2_400_000);
        bus.write(&encode(&[0x12, 0x34, 0x56, 0xab, 0xcd, 0xef]))
            .expect("frames");
        assert_eq!(
            hardware.read_spi_registers("SPI2", 0, 6).expect("peek"),
            [0; 6]
        );
        bus.write(&[0; 24]).expect("reset");
        assert_eq!(
            hardware.read_spi_registers("SPI2", 0, 6).expect("peek"),
            [0x12, 0x34, 0x56, 0xab, 0xcd, 0xef]
        );
        assert!(
            hardware.violations().is_empty(),
            "{:?}",
            hardware.violations()
        );
    }

    #[test]
    fn an_idle_gap_longer_than_res_also_latches() {
        let (hardware, mut bus) = setup(2_400_000);
        // 72 SPI bits take 30 µs on the wire; the line then idles low.
        bus.write(&encode(&[1, 2, 3])).expect("frame");
        hardware.clock().advance(Duration::from_micros(30 + 51));
        bus.write(&[0]).expect("next");
        assert_eq!(
            hardware.read_spi_registers("SPI2", 0, 3).expect("peek"),
            [1, 2, 3]
        );
    }

    #[test]
    fn a_clock_outside_the_code_windows_and_partial_frames_are_reported() {
        let (hardware, mut bus) = setup(3_200_000);
        bus.write(&encode(&[0])).expect("short low times");
        bus.write(&[0; 32]).expect("reset");
        let rules: Vec<_> = hardware
            .violations()
            .into_iter()
            .map(|violation| violation.violation.rule)
            .collect();
        assert!(
            rules.contains(&String::from(rules::BIT_TIMING)),
            "{rules:?}"
        );
        assert!(rules.contains(&String::from(rules::FRAME)), "{rules:?}");
    }
}

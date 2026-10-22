//! Register-level driver for the Lite-On LTR-553ALS ambient-light and
//! proximity sensor.
//!
//! Registers follow the LTR-553ALS-01 datasheet, as Zephyr's `ltr55x`
//! driver applies it: `ALS_CONTR` (`80h`, bit 0 active, bits 4:2 gain),
//! `PS_CONTR` (`81h`, bits 1:0 `10b` active), `ALS_MEAS_RATE` (`85h`,
//! bits 5:3 integration time, bits 2:0 repeat rate), `PART_ID` (`86h`,
//! `92h`), `MANUFAC_ID` (`87h`, `05h`), and the data block from `88h`:
//! channel 1 and channel 0 (little-endian), `ALS_PS_STATUS`, and the
//! 11-bit proximity count. After power-up the sensor needs 100 ms before it
//! answers, and 10 ms to leave standby.
//!
//! Illuminance follows the datasheet's appendix A: with the channel ratio
//! `CH1 / (CH0 + CH1)`, lux is `(1.7743 CH0 + 1.1059 CH1)` below 0.45,
//! `(4.2785 CH0 - 1.9548 CH1)` below 0.64, `(0.5926 CH0 + 0.1185 CH1)`
//! below 0.85, and 0 above, each divided by the gain and by the
//! integration time in units of 100 ms.
//!
//! No upstream Rust crate drives this chip.

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// The LTR-553ALS's fixed address.
pub const ADDRESS: u8 = 0x23;

const ALS_CONTR: u8 = 0x80;
const PS_CONTR: u8 = 0x81;
const ALS_MEAS_RATE: u8 = 0x85;
const PART_ID: u8 = 0x86;
const ALS_DATA_CH1_0: u8 = 0x88;
/// `PART_ID` of the LTR-553ALS (part 9, revision 2).
const PART_ID_VALUE: u8 = 0x92;
/// `MANUFAC_ID` of Lite-On.
const MANUFACTURER_ID_VALUE: u8 = 0x05;
const ALS_ACTIVE: u8 = 0x01;
const PS_ACTIVE: u8 = 0x02;
/// 100 ms integration, 500 ms repeat rate: the power-on default.
const MEASUREMENT_RATE: u8 = 0x03;
/// Integration time in tenths of 100 ms for `MEASUREMENT_RATE`.
const INTEGRATION_TENTHS: u64 = 10;
/// Time from power-up until the sensor answers.
pub const STARTUP_MS: u32 = 100;
/// Time from standby until the first conversion starts.
const WAKEUP_MS: u32 = 10;
const PROXIMITY_MASK: u16 = 0x07ff;

/// Ambient-light gain; higher gains resolve dimmer light over a smaller range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlsGain {
    /// 1x: 1 to 64k lux.
    X1,
    /// 2x: 0.5 to 32k lux.
    X2,
    /// 4x: 0.25 to 16k lux.
    X4,
    /// 8x: 0.125 to 8k lux.
    X8,
    /// 48x: 0.02 to 1.3k lux.
    X48,
    /// 96x: 0.01 to 600 lux.
    X96,
}

impl AlsGain {
    const fn bits(self) -> u8 {
        match self {
            Self::X1 => 0b000,
            Self::X2 => 0b001,
            Self::X4 => 0b010,
            Self::X8 => 0b011,
            Self::X48 => 0b110,
            Self::X96 => 0b111,
        }
    }

    const fn factor(self) -> u64 {
        match self {
            Self::X1 => 1,
            Self::X2 => 2,
            Self::X4 => 4,
            Self::X8 => 8,
            Self::X48 => 48,
            Self::X96 => 96,
        }
    }
}

/// One ambient-light and proximity sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Illuminance in millilux.
    pub illuminance_millilux: u32,
    /// Proximity in raw counts, 0 through 2047; larger is nearer.
    pub proximity_counts: u16,
}

/// LTR-553ALS transport or identity failure.
#[derive(Debug)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// The part or manufacturer ID is not the LTR-553ALS's.
    WrongPart {
        /// The part ID read.
        part: u8,
        /// The manufacturer ID read.
        manufacturer: u8,
    },
}

/// An LTR-553ALS at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ltr553 {
    address: u8,
    gain: AlsGain,
}

impl Ltr553 {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8, gain: AlsGain) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address, gain })
        } else {
            None
        }
    }

    /// Checks the chip's identity, then starts the ambient-light and
    /// proximity sensors and waits for them to leave standby.
    ///
    /// # Errors
    ///
    /// Returns a bus error or [`Error::WrongPart`].
    pub fn start<I2C: I2c, D: DelayNs>(
        &self,
        i2c: &mut I2C,
        delay: &mut D,
    ) -> Result<(), Error<I2C::Error>> {
        let mut identity = [0; 2];
        i2c.write_read(self.address, &[PART_ID], &mut identity)
            .map_err(Error::Bus)?;
        let [part, manufacturer] = identity;
        if part != PART_ID_VALUE || manufacturer != MANUFACTURER_ID_VALUE {
            return Err(Error::WrongPart { part, manufacturer });
        }
        i2c.write(self.address, &[ALS_MEAS_RATE, MEASUREMENT_RATE])
            .map_err(Error::Bus)?;
        i2c.write(
            self.address,
            &[ALS_CONTR, self.gain.bits().wrapping_shl(2) | ALS_ACTIVE],
        )
        .map_err(Error::Bus)?;
        i2c.write(self.address, &[PS_CONTR, PS_ACTIVE])
            .map_err(Error::Bus)?;
        delay.delay_ms(WAKEUP_MS);
        Ok(())
    }

    /// Reads the latest ambient-light and proximity conversions in one
    /// transfer, as the datasheet requires for the channel registers.
    ///
    /// # Errors
    ///
    /// Returns the bus error.
    pub fn read<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Sample, I2C::Error> {
        let mut data = [0; 7];
        i2c.write_read(self.address, &[ALS_DATA_CH1_0], &mut data)?;
        let [
            ch1_low,
            ch1_high,
            ch0_low,
            ch0_high,
            _status,
            ps_low,
            ps_high,
        ] = data;
        let ch1 = u16::from_le_bytes([ch1_low, ch1_high]);
        let ch0 = u16::from_le_bytes([ch0_low, ch0_high]);
        Ok(Sample {
            illuminance_millilux: illuminance_millilux(ch0, ch1, self.gain),
            proximity_counts: u16::from_le_bytes([ps_low, ps_high]) & PROXIMITY_MASK,
        })
    }
}

/// Converts the two channels to millilux with the datasheet's appendix A.
#[must_use]
pub fn illuminance_millilux(ch0: u16, ch1: u16, gain: AlsGain) -> u32 {
    let (ch0, ch1) = (u64::from(ch0), u64::from(ch1));
    let total = ch0.saturating_add(ch1);
    // Ratio thresholds compared as CH1 * 100 against total * percent.
    let ratio_below = |percent: u64| ch1.saturating_mul(100) < total.saturating_mul(percent);
    // Coefficients in ten-thousandths.
    let scaled = if total == 0 {
        0
    } else if ratio_below(45) {
        ch0.saturating_mul(17_743)
            .saturating_add(ch1.saturating_mul(11_059))
    } else if ratio_below(64) {
        ch0.saturating_mul(42_785)
            .saturating_sub(ch1.saturating_mul(19_548))
    } else if ratio_below(85) {
        ch0.saturating_mul(5_926)
            .saturating_add(ch1.saturating_mul(1_185))
    } else {
        0
    };
    // lux = scaled / 10_000 / gain / (integration / 100 ms), so
    // millilux = scaled * 1_000 * 10 / 10_000 / gain / integration_tenths.
    let millilux = scaled
        .checked_div(gain.factor().saturating_mul(INTEGRATION_TENTHS))
        .unwrap_or(0);
    u32::try_from(millilux).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;
    use std::{vec, vec::Vec};

    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{ADDRESS, AlsGain, Error, Ltr553, Sample, illuminance_millilux};

    /// An auto-incrementing register file from `80h`.
    struct Sensor {
        registers: [u8; 0x20],
        pointer: usize,
        writes: Vec<Vec<u8>>,
    }

    impl Sensor {
        fn new() -> Self {
            let mut registers = [0; 0x20];
            registers[0x06] = 0x92;
            registers[0x07] = 0x05;
            Self {
                registers,
                pointer: 0,
                writes: Vec::new(),
            }
        }
    }

    impl ErrorType for Sensor {
        type Error = Infallible;
    }

    impl I2c for Sensor {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, ADDRESS);
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        self.pointer = usize::from(bytes[0] - 0x80);
                        for (offset, value) in bytes[1..].iter().enumerate() {
                            self.registers[self.pointer + offset] = *value;
                        }
                        if bytes.len() > 1 {
                            self.writes.push(bytes.to_vec());
                        }
                    }
                    Operation::Read(buffer) => {
                        let start = self.pointer;
                        buffer.copy_from_slice(&self.registers[start..start + buffer.len()]);
                    }
                }
            }
            Ok(())
        }
    }

    struct Delay;

    impl DelayNs for Delay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    #[test]
    fn starts_both_sensors_and_reads_one_block() {
        let mut bus = Sensor::new();
        let sensor = Ltr553::new(ADDRESS, AlsGain::X1).expect("valid address");
        sensor.start(&mut bus, &mut Delay).expect("start");
        assert_eq!(
            bus.writes,
            [vec![0x85, 0x03], vec![0x80, 0x01], vec![0x81, 0x02]]
        );
        // CH1 = 100, CH0 = 400, status, proximity 0x0123 with the
        // saturation bit set.
        bus.registers[0x08..0x0f].copy_from_slice(&[100, 0, 0x90, 0x01, 0x05, 0x23, 0x81]);
        assert_eq!(
            sensor.read(&mut bus).expect("read"),
            Sample {
                // Ratio 0.2: 1.7743 * 400 + 1.1059 * 100 = 820.31 lux.
                illuminance_millilux: 820_310,
                proximity_counts: 0x0123,
            }
        );
    }

    #[test]
    fn applies_each_ratio_band_and_the_gain() {
        // Ratio 0.5: 4.2785 * 100 - 1.9548 * 100 = 232.37 lux.
        assert_eq!(illuminance_millilux(100, 100, AlsGain::X1), 232_370);
        // Ratio 0.75: 0.5926 * 100 + 0.1185 * 300 = 94.81 lux.
        assert_eq!(illuminance_millilux(100, 300, AlsGain::X1), 94_810);
        // Ratio 0.9 and darkness read zero.
        assert_eq!(illuminance_millilux(10, 90, AlsGain::X1), 0);
        assert_eq!(illuminance_millilux(0, 0, AlsGain::X1), 0);
        // The same counts at 96x gain are 96 times less light.
        assert_eq!(illuminance_millilux(9_600, 0, AlsGain::X96), 177_430);
    }

    #[test]
    fn refuses_another_part() {
        let mut bus = Sensor::new();
        bus.registers[0x06] = 0xa0;
        let sensor = Ltr553::new(ADDRESS, AlsGain::X1).expect("valid address");
        assert!(matches!(
            sensor.start(&mut bus, &mut Delay),
            Err(Error::WrongPart {
                part: 0xa0,
                manufacturer: 0x05
            })
        ));
        assert!(Ltr553::new(0x80, AlsGain::X1).is_none());
    }
}

//! Register driver for the Lontium LT8618SX RGB-to-HDMI transmitter.
//!
//! Lontium publishes no register manual; the sequences here are M5GFX's
//! (`Panel_M5HDMI::HDMI_Trans`, M5GFX 0.2.34), which configures the
//! transmitter behind M5Stack's HDMI products for 24-bit RGB input and
//! 48 kHz audio. The chip pages its registers: writing register FFh selects
//! the bank (80h, 81h, 82h, 84h) the following addresses refer to.
//!
//! M5GFX sends every write of a sequence even when one reports a missing
//! acknowledge, because on some products the transmitter's acknowledge
//! does not reach 0 V and is misread although the write was accepted; the
//! outcome is judged by reading back (the chip ID, the PLL lock). This
//! driver does the same.

#![no_std]

use embedded_hal::i2c::I2c;

/// The transmitter's seven-bit address on M5Stack's HDMI products (M5GFX
/// `cfg.i2c_addr = 0x39`).
pub const DEFAULT_ADDRESS: u8 = 0x39;

/// Bank select register.
const BANK: u8 = 0xff;
/// PLL attempts before giving up (M5GFX: eight).
const PLL_ATTEMPTS: usize = 8;

/// M5GFX `HDMI_Trans::reset`: register/value pairs that stop the output.
const RESET: [u8; 38] = [
    0xff, 0x81, 0x30, 0x00, 0x02, 0x66, 0x0a, 0x06, 0x15, 0x06, 0x4e, 0xa8, 0xff, 0x80, 0xee, 0x01,
    0x11, 0x00, 0x13, 0xf1, 0x13, 0xf9, 0x0a, 0x80, 0xff, 0x82, 0x1b, 0x77, 0x1c, 0xec, 0x45, 0x00,
    0x4f, 0x40, 0x50, 0x00, 0x47, 0x07,
];

/// M5GFX `HDMI_Trans::init` `data_1`: audio at 48 kHz, then the TX PHY.
const INIT: [u8; 54] = [
    0xff, 0x82, 0xd6, 0x8e, 0xd7, 0x04, 0xff, 0x84, 0x06, 0x08, 0x07, 0x10, 0x09, 0x00, 0x0f, 0x2b,
    0x34, 0xd5, 0x35, 0x00, 0x36, 0x18, 0x37, 0x00, 0x3c, 0x21, 0xff, 0x82, 0xde, 0x00, 0xde, 0xc0,
    0xff, 0x81, 0x23, 0x40, 0x24, 0x64, 0x26, 0x55, 0x29, 0x04, 0x4d, 0x00, 0x27, 0x60, 0x28, 0x00,
    0x25, 0x01, 0x2c, 0x94, 0x2d, 0x99,
];

/// M5GFX `data_u3`: the PLL settings of chip revision U3 (ID byte 2 = E2h).
const INIT_U3: [u8; 16] = [
    0x4d, 0x09, 0x27, 0x66, 0x28, 0x88, 0x2a, 0x00, 0x2a, 0x20, 0x25, 0x00, 0x2c, 0x9e, 0x2d, 0x99,
];

/// M5GFX `data_pll`: restart the PLL calibration, ending in bank 82h.
const PLL: [u8; 16] = [
    0xff, 0x80, 0x16, 0xf1, 0x18, 0xdc, 0x18, 0xfc, 0x16, 0xf3, 0x16, 0xe3, 0x16, 0xf3, 0xff, 0x82,
];

/// M5GFX `init` final `data`: enable the output once the PLL has locked.
const ENABLE_OUTPUT: [u8; 56] = [
    0xb9, 0x00, 0xff, 0x84, 0x43, 0x31, 0x44, 0x10, 0x45, 0x2a, 0x47, 0x04, 0x10, 0x2c, 0x12, 0x64,
    0x3d, 0x0a, 0xff, 0x80, 0x11, 0x00, 0x13, 0xf1, 0x13, 0xf9, 0xff, 0x81, 0x31, 0x44, 0x32, 0x4a,
    0x33, 0x0b, 0x34, 0x00, 0x35, 0x00, 0x36, 0x00, 0x37, 0x44, 0x3f, 0x0f, 0x40, 0xa0, 0x41, 0xa0,
    0x42, 0xa0, 0x43, 0xa0, 0x44, 0xa0, 0x30, 0xea,
];

/// Chip ID registers 00h-02h in bank 80h.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChipId(pub [u8; 3]);

impl ChipId {
    /// Whether the ID came from a chip: M5GFX treats three equal bytes as
    /// no transmitter.
    #[must_use]
    pub const fn is_present(self) -> bool {
        let [a, b, c] = self.0;
        !(a == b && a == c)
    }

    /// Whether the chip is revision U3, which needs its own PLL settings.
    #[must_use]
    pub const fn is_u3(self) -> bool {
        self.0[2] == 0xe2
    }
}

/// Transport or bring-up failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// A register read failed.
    Bus(E),
    /// No transmitter answered.
    NotPresent(ChipId),
    /// The TX PLL did not lock in eight attempts.
    PllNotLocked,
}

/// An LT8618SX at a caller-selected seven-bit address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lt8618sx {
    address: u8,
}

impl Lt8618sx {
    /// Creates a driver when `address` is a valid seven-bit I2C address.
    #[must_use]
    pub const fn new(address: u8) -> Option<Self> {
        if address <= 0x7f {
            Some(Self { address })
        } else {
            None
        }
    }

    /// Reads the chip ID (bank 80h, registers 00h-02h, after enabling
    /// register access with EEh = 01h).
    ///
    /// # Errors
    ///
    /// Returns the bus error of a failed read.
    pub fn chip_id<I2C: I2c>(&self, i2c: &mut I2C) -> Result<ChipId, I2C::Error> {
        self.write_sequence(i2c, &[BANK, 0x80, 0xee, 0x01]);
        let mut id = [0u8; 3];
        for (register, value) in (0u8..).zip(id.iter_mut()) {
            *value = self.read_register(i2c, register)?;
        }
        Ok(ChipId(id))
    }

    /// Stops the HDMI output (M5GFX `HDMI_Trans::reset`).
    pub fn reset<I2C: I2c>(&self, i2c: &mut I2C) {
        self.write_sequence(i2c, &RESET);
    }

    /// Configures the transmitter and enables its output once the TX PLL
    /// locks (M5GFX `HDMI_Trans::init`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotPresent`] when no chip answers, [`Error::PllNotLocked`]
    /// when the PLL never locks, and [`Error::Bus`] for a failed read.
    pub fn init<I2C: I2c>(&self, i2c: &mut I2C) -> Result<(), Error<I2C::Error>> {
        let id = self.chip_id(i2c).map_err(Error::Bus)?;
        if !id.is_present() {
            return Err(Error::NotPresent(id));
        }
        self.write_sequence(i2c, &INIT);
        let value = self.read_register(i2c, 0x2b).map_err(Error::Bus)?;
        self.write_sequence(i2c, &[0x2b, value & 0xfd]);
        let value = self.read_register(i2c, 0x2e).map_err(Error::Bus)?;
        self.write_sequence(i2c, &[0x2e, value & 0xfe]);
        if id.is_u3() {
            self.write_sequence(i2c, &INIT_U3);
        }
        for _ in 0..PLL_ATTEMPTS {
            self.write_sequence(i2c, &PLL);
            let locked = self.read_register(i2c, 0x15).map_err(Error::Bus)? & 0x80 != 0;
            let value = self.read_register(i2c, 0xea).map_err(Error::Bus)?;
            let done = self.read_register(i2c, 0xeb).map_err(Error::Bus)? & 0x80 != 0;
            if locked && done && value != 0xff {
                self.write_sequence(i2c, &ENABLE_OUTPUT);
                return Ok(());
            }
        }
        Err(Error::PllNotLocked)
    }

    /// Writes register/value pairs, all of them whatever one reports.
    fn write_sequence<I2C: I2c>(&self, i2c: &mut I2C, pairs: &[u8]) {
        for pair in pairs.as_chunks::<2>().0 {
            // A reported failure is often a misread acknowledge of an
            // accepted write; the caller judges by reading back.
            let _ = i2c.write(self.address, pair);
        }
    }

    fn read_register<I2C: I2c>(&self, i2c: &mut I2C, register: u8) -> Result<u8, I2C::Error> {
        let mut value = [0u8; 1];
        i2c.write_read(self.address, &[register], &mut value)?;
        Ok(value[0])
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{ChipId, Error, Lt8618sx};

    /// A banked register file; records every write as (bank, register, value).
    struct Transmitter {
        bank: u8,
        registers: [[u8; 256]; 256],
        writes: Vec<(u8, u8, u8)>,
        lock_after: usize,
        pll_runs: usize,
    }

    impl Transmitter {
        fn new(id: [u8; 3], lock_after: usize) -> std::boxed::Box<Self> {
            let mut chip = std::boxed::Box::new(Self {
                bank: 0,
                registers: [[0; 256]; 256],
                writes: Vec::new(),
                lock_after,
                pll_runs: 0,
            });
            chip.registers[0x80][..3].copy_from_slice(&id);
            chip.registers[0x81][0x2b] = 0xff;
            chip.registers[0x81][0x2e] = 0xff;
            chip
        }
    }

    impl ErrorType for Transmitter {
        type Error = Infallible;
    }

    impl I2c for Transmitter {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x39);
            let mut register = 0usize;
            for operation in operations {
                match operation {
                    Operation::Write([0xff, bank]) => self.bank = *bank,
                    Operation::Write([reg, value]) => {
                        self.writes.push((self.bank, *reg, *value));
                        self.registers[usize::from(self.bank)][usize::from(*reg)] = *value;
                        // The last write of the PLL restart.
                        if self.bank == 0x80 && *reg == 0x16 && *value == 0xf3 {
                            self.pll_runs += 1;
                        }
                    }
                    Operation::Write([reg]) => register = usize::from(*reg),
                    Operation::Read([value]) => {
                        *value = if self.bank == 0x82 && self.pll_runs >= 2 * self.lock_after {
                            match register {
                                0x15 | 0xeb => 0x80,
                                0xea => 0x12,
                                _ => self.registers[0x82][register],
                            }
                        } else {
                            self.registers[usize::from(self.bank)][register]
                        };
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }

    #[test]
    fn three_equal_id_bytes_mean_no_transmitter() {
        assert!(!ChipId([0xff; 3]).is_present());
        assert!(ChipId([0x17, 0x02, 0xe2]).is_present());
        let mut chip = Transmitter::new([0; 3], 1);
        let transmitter = Lt8618sx::new(0x39).expect("address");
        assert_eq!(
            transmitter.init(&mut *chip),
            Err(Error::NotPresent(ChipId([0; 3])))
        );
    }

    #[test]
    fn init_clears_the_two_bits_and_enables_output_once_locked() {
        let mut chip = Transmitter::new([0x17, 0x02, 0xe1], 3);
        let transmitter = Lt8618sx::new(0x39).expect("address");
        transmitter.init(&mut *chip).expect("init");
        assert_eq!(chip.registers[0x81][0x2b], 0xfd);
        assert_eq!(chip.registers[0x81][0x2e], 0xfe);
        // Not U3: no `data_u3` value written to 4Dh in bank 81h.
        assert!(!chip.writes.contains(&(0x81, 0x4d, 0x09)));
        // The output was enabled with the final 30h = EAh in bank 81h.
        assert_eq!(chip.writes.last(), Some(&(0x81, 0x30, 0xea)));
    }

    #[test]
    fn a_u3_chip_gets_its_pll_settings() {
        let mut chip = Transmitter::new([0x17, 0x02, 0xe2], 1);
        let transmitter = Lt8618sx::new(0x39).expect("address");
        transmitter.init(&mut *chip).expect("init");
        assert!(chip.writes.contains(&(0x81, 0x4d, 0x09)));
    }

    #[test]
    fn an_unlocked_pll_fails_after_eight_attempts() {
        let mut chip = Transmitter::new([0x17, 0x02, 0xe1], 100);
        let transmitter = Lt8618sx::new(0x39).expect("address");
        assert_eq!(transmitter.init(&mut *chip), Err(Error::PllNotLocked));
    }

    #[test]
    fn reset_stops_the_output() {
        let mut chip = Transmitter::new([0x17, 0x02, 0xe1], 1);
        Lt8618sx::new(0x39).expect("address").reset(&mut *chip);
        assert_eq!(chip.writes.first(), Some(&(0x81, 0x30, 0x00)));
        assert_eq!(chip.writes.last(), Some(&(0x82, 0x47, 0x07)));
    }
}

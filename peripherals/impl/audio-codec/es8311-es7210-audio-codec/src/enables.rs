//! Audio supply and amplifier enables behind M5Stack's helper chips.
//!
//! The M5IOE1 expander (0x4F) and the M5PM1 power controller (0x6E) sit on
//! the codec's control bus on the StickS3, StopWatch, Chain Captain, and
//! CoreP4X, and switch the audio supply and the speaker amplifier there
//! (M5Unified `_speaker_enabled_cb_sticks3`, `_stopwatch`, `_chain_captain`,
//! `_corep4x`). They stay private to the codec implementation.

use barracuda_driver_m5ioe1::{self as m5ioe1, M5ioe1};
use barracuda_driver_m5pm1::{self as m5pm1, M5pm1};
use embedded_hal::i2c::I2c;

/// Which helper-chip pins switch the audio parts; zero (M5IOE1) or a
/// negative number (M5PM1) means none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HelperEnables {
    m5ioe1_supply: u8,
    m5ioe1_amplifier: u8,
    m5pm1_amplifier: Option<u8>,
}

impl HelperEnables {
    /// Creates the enables from Board parameters: M5IOE1 pins 1 to 14 for
    /// the audio supply and the amplifier, M5PM1 pin 0 to 4 for the
    /// amplifier.
    #[must_use]
    pub const fn new(m5ioe1_supply: i64, m5ioe1_amplifier: i64, m5pm1_amplifier: i64) -> Self {
        Self {
            m5ioe1_supply: pin(m5ioe1_supply),
            m5ioe1_amplifier: pin(m5ioe1_amplifier),
            m5pm1_amplifier: if m5pm1_amplifier >= 0 && m5pm1_amplifier < 5 {
                Some(m5pm1_amplifier as u8)
            } else {
                None
            },
        }
    }

    /// Whether any helper-chip pin is named.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.m5ioe1_supply == 0 && self.m5ioe1_amplifier == 0 && self.m5pm1_amplifier.is_none()
    }

    /// Switches the audio supply on, before the codec is programmed.
    ///
    /// # Errors
    ///
    /// Returns the M5IOE1's register error.
    pub fn supply_on<I2C: I2c>(self, i2c: &mut I2C) -> Result<(), HelperError<I2C::Error>> {
        if self.m5ioe1_supply != 0 {
            let mut expander =
                M5ioe1::new(i2c, m5ioe1::DEFAULT_ADDRESS).map_err(HelperError::M5ioe1)?;
            expander.wake().map_err(HelperError::M5ioe1)?;
            expander
                .configure_output(self.m5ioe1_supply, true)
                .map_err(HelperError::M5ioe1)?;
        }
        Ok(())
    }

    /// Switches the speaker amplifier on, after the codec is programmed.
    ///
    /// # Errors
    ///
    /// Returns the helper chip's register error.
    pub fn amplifier_on<I2C: I2c>(self, i2c: &mut I2C) -> Result<(), HelperError<I2C::Error>> {
        if self.m5ioe1_amplifier != 0 {
            let mut expander =
                M5ioe1::new(i2c, m5ioe1::DEFAULT_ADDRESS).map_err(HelperError::M5ioe1)?;
            expander.wake().map_err(HelperError::M5ioe1)?;
            expander
                .configure_output(self.m5ioe1_amplifier, true)
                .map_err(HelperError::M5ioe1)?;
        }
        if let Some(pin) = self.m5pm1_amplifier {
            let mut controller =
                M5pm1::new(i2c, m5pm1::DEFAULT_ADDRESS).map_err(HelperError::M5pm1)?;
            controller.disable_i2c_sleep().map_err(HelperError::M5pm1)?;
            controller
                .configure_output(pin, true)
                .map_err(HelperError::M5pm1)?;
        }
        Ok(())
    }
}

const fn pin(value: i64) -> u8 {
    if value > 0 && value <= 14 {
        value as u8
    } else {
        0
    }
}

/// A helper chip rejected an operation.
#[derive(Debug)]
pub enum HelperError<BusError> {
    /// The M5IOE1 expander failed.
    M5ioe1(m5ioe1::Error<BusError>),
    /// The M5PM1 power controller failed.
    M5pm1(m5pm1::Error<BusError>),
}

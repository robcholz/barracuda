//! Display implementation for the M5Stack PaperDIY.
//!
//! The PaperDIY wires the same ED047TC1 4.7-inch panel and source/gate lines
//! as the PaperS3, but switches the e-paper supply from its M5PM1 power
//! controller. As M5GFX does for `board_M5PaperDIY`, this implementation
//! keeps the M5PM1's I2C awake, stops its watchdog, makes its G2 a push-pull
//! output driven high, and waits 10 ms; the panel is then driven by
//! [`barracuda_ed047tc1_display`].

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_m5pm1::{self as m5pm1, M5pm1};
use barracuda_ed047tc1_display::{
    Ed047tc1Bindings, Ed047tc1Display, Ed047tc1DisplayConfig, Ed047tc1Error,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    display::{DisplayOrientation, ParallelOutput},
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c};

/// M5PM1 pin switching the e-paper supply.
pub const PM1_EPD_SUPPLY: u8 = 2;
/// Settle time after the supply rises (M5GFX: 10 ms).
const SUPPLY_SETTLE_MS: u32 = 10;

/// M5PM1 address and panel mounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaperDiyConfig {
    m5pm1: u8,
    panel: Ed047tc1DisplayConfig,
}

impl PaperDiyConfig {
    /// Selects the M5PM1 address (falling back to `0x6E` when out of range)
    /// and the panel mounting.
    #[must_use]
    pub fn new(m5pm1: i64, mounting: DisplayOrientation) -> Self {
        Self {
            m5pm1: u8::try_from(m5pm1).unwrap_or(m5pm1::DEFAULT_ADDRESS),
            panel: Ed047tc1DisplayConfig::new(mounting),
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct PaperDiyBindings<BUS, P, I2C, DELAY> {
    panel: Ed047tc1Bindings<BUS, P, DELAY>,
    i2c: I2C,
    delay: DELAY,
}

impl<BUS, P: OutputPin, I2C, DELAY> PaperDiyBindings<BUS, P, I2C, DELAY> {
    /// Combines the panel's bus and timing outputs with the internal I2C bus;
    /// the second delay paces the M5PM1 bring-up.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        bus: BUS,
        spv: P,
        ckv: P,
        le: P,
        oe: P,
        pwr: P,
        i2c: I2C,
        panel_delay: DELAY,
        delay: DELAY,
    ) -> Self {
        Self {
            panel: Ed047tc1Bindings::new(bus, spv, ckv, le, oe, pwr, panel_delay),
            i2c,
            delay,
        }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum PaperDiyError<BUS, PIN, I2C> {
    /// The panel could not be set up.
    Panel(Ed047tc1Error<BUS, PIN>),
    /// The M5PM1 rejected a register access.
    PowerController(m5pm1::Error<I2C>),
}

/// Static implementation factory.
pub struct PaperDiyDisplayImplementation<BUS, P, I2C, DELAY>(PhantomData<(BUS, P, I2C, DELAY)>);

impl<BUS, P, I2C, DELAY> PeripheralImplementation
    for PaperDiyDisplayImplementation<BUS, P, I2C, DELAY>
where
    BUS: ParallelOutput + 'static,
    P: OutputPin + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
    BUS::Error: 'static,
    P::Error: 'static,
    I2C::Error: 'static,
{
    type Bindings = PaperDiyBindings<BUS, P, I2C, DELAY>;
    type Config = PaperDiyConfig;
    type Peripheral = Ed047tc1Display<BUS, P, DELAY>;
    type Error = PaperDiyError<BUS::Error, P::Error, I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let PaperDiyBindings {
            panel,
            mut i2c,
            mut delay,
        } = bindings;
        {
            let mut pm1 =
                M5pm1::new(&mut i2c, config.m5pm1).map_err(PaperDiyError::PowerController)?;
            pm1.disable_i2c_sleep()
                .map_err(PaperDiyError::PowerController)?;
            pm1.disable_watchdog()
                .map_err(PaperDiyError::PowerController)?;
            pm1.configure_output(PM1_EPD_SUPPLY)
                .map_err(PaperDiyError::PowerController)?;
            pm1.set_output(PM1_EPD_SUPPLY, true)
                .map_err(PaperDiyError::PowerController)?;
        }
        delay.delay_ms(SUPPLY_SETTLE_MS);
        Ed047tc1Display::new(panel, config.panel).map_err(PaperDiyError::Panel)
    }
}

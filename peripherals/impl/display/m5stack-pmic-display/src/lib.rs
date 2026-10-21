//! Display implementation for M5Stack SPI LCDs whose supply, reset, and
//! backlight come from an AXP192 or AXP2101 PMIC and, on the CoreS3, an
//! AW9523B expander.
//!
//! The panels are ordinary MIPI DBI controllers driven through
//! `barracuda-mipi-dbi-display`. This implementation adds the helper chips:
//! before the controller is initialized it switches on the LCD rail and
//! pulses the reset line the Board's power layout puts behind the PMIC or the
//! expander, and it dims the backlight by programming the regulator that feeds
//! it. Neither chip is visible outside the display.
//!
//! The display owns only the PMIC's regulator, voltage, and GPIO registers.
//! Its measurement registers (status, ADC, fuel gauge) belong to
//! `axp192-axp2101-power-monitor`, which reaches the PMIC through its own
//! view of the shared bus; neither implementation writes the other's
//! registers.
//!
//! Rails, reset lines, and brightness curves follow M5Stack's M5GFX
//! (`Light_M5StackCore2`, `Light_M5StackCore2_AXP2101`, `Light_M5Tough`,
//! `Light_M5StickC`, `Light_M5StackCoreS3`, and the board autodetection) and
//! M5Unified (`Power_Class::begin`).

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_aw9523::{Aw9523, Error as Aw9523Error};
use barracuda_driver_axp192::{Axp192, Error as Axp192Error, Gpio as Axp192Gpio, Rail};
use barracuda_driver_axp2101::{Axp2101, Error as Axp2101Error, Ldo};
use barracuda_mipi_dbi_display::{MipiDbiDisplay, MipiDbiInitError, PanelBacklight};
use barracuda_peripheral::PeripheralImplementation;
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c, spi::SpiDevice};
use mipidsi::{
    interface::{InterfacePixelFormat, SpiError, SpiInterface},
    models::Model,
};

/// Portable panel options and controller models used by generated configuration.
pub use barracuda_mipi_dbi_display::{
    ILI9342CRgb565, MipiDbiColorOrder, MipiDbiDisplayConfig, MipiDbiDriverConfig, PanelReset,
    ST7735s, ST7789, Size,
};

/// Seven-bit address shared by the AXP192 and AXP2101 on every supported layout.
const PMIC_ADDRESS: u8 = 0x34;
/// M5GFX `aw9523_i2c_addr`: the CoreS3 AW9523B with AD0 and AD1 low.
const EXPANDER_ADDRESS: u8 = 0x58;
/// M5GFX `Panel_M5StackCoreS3_T::rst_control`: LCD_RST is AW9523B P1_1.
const CORES3_LCD_RESET_PIN: u8 = 9;

/// Reset pulse applied through a helper chip: ILI9342C needs at least 10 us
/// low; the ILI9342E reloads its registers for up to 120 ms after release
/// (M5GFX, ILI9342E datasheet 12.4), so commands wait that long.
const RESET_LOW_MS: u32 = 10;
const RESET_SETTLE_MS: u32 = 120;
/// Time for a freshly switched LCD rail to settle before the reset pulse.
const POWER_SETTLE_MS: u32 = 10;

/// Takes the statically allocated transfer buffer used by one generated panel.
///
/// Generated Board initialization constructs this display once, so the
/// returned buffer has one move-only owner for the lifetime of the application.
#[must_use]
pub fn take_transfer_buffer() -> &'static mut [u8; 512] {
    static BUFFER: static_cell::StaticCell<[u8; 512]> = static_cell::StaticCell::new();
    BUFFER.init([0; 512])
}

/// Which PMIC and expander outputs power, reset, and light the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerLayout {
    /// M5Stack Core2 and Core2 for AWS. The original revision has an AXP192:
    /// LDO2 powers the LCD, GPIO4 resets it, and DC-DC3 lights it. Core2 v1.1
    /// has an AXP2101: ALDO4 powers the LCD, ALDO2 resets it, and BLDO1 lights
    /// it. The fitted PMIC is identified at initialization.
    Core2,
    /// M5Stack Tough: AXP192 LDO2 powers the LCD, GPIO4 resets it, and LDO3
    /// lights it.
    Tough,
    /// M5StickC and M5StickC Plus: AXP192 LDO3 powers the LCD at 3.0 V and
    /// LDO2 lights it; the reset line is an ESP32 GPIO.
    StickC,
    /// M5Stack Station: AXP192 LDO2 powers the LCD and LDO3 lights it; the
    /// reset line is an ESP32 GPIO.
    Station,
    /// M5Stack CoreS3, CoreS3 SE, and StackChan: AW9523B P1_1 resets the LCD
    /// and AXP2101 DLDO1 lights it.
    CoreS3,
}

impl PowerLayout {
    /// Whether the Board wires the LCD reset to an ESP32 GPIO instead of a
    /// helper chip.
    #[must_use]
    pub const fn has_gpio_reset(self) -> bool {
        matches!(self, Self::StickC | Self::Station)
    }
}

/// Regulator that feeds the LED backlight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BacklightRail {
    /// AXP192 DC-DC3 (Core2).
    Axp192Dcdc3,
    /// AXP192 LDO2 (StickC, StickC Plus).
    Axp192Ldo2,
    /// AXP192 LDO3 (Tough, Station).
    Axp192Ldo3,
    /// AXP2101 BLDO1 (Core2 v1.1).
    Axp2101Bldo1,
    /// AXP2101 DLDO1 (CoreS3).
    Axp2101Dldo1,
}

impl BacklightRail {
    /// Converts a non-zero portable brightness into the rail voltage M5GFX
    /// programs for it. Zero switches the rail off instead.
    #[must_use]
    pub const fn millivolts(self, brightness: u8) -> u16 {
        let level = brightness as u16;
        match self {
            // Light_M5StackCore2: code (b >> 3) + 72 at 25 mV above 700 mV.
            Self::Axp192Dcdc3 => 2500 + (level >> 3) * 25,
            // Light_M5StickC: code ((b >> 1) + 8) / 13 + 5 at 100 mV above 1.8 V.
            Self::Axp192Ldo2 => 1800 + (((level >> 1) + 8) / 13 + 5) * 100,
            // Light_M5Tough: code 7 + b * 9 / 256; below 2.5 V the LEDs stay dark.
            Self::Axp192Ldo3 => 1800 + (7 + level * 9 / 256) * 100,
            // Light_M5StackCore2_AXP2101 and Light_M5StackCoreS3:
            // code (b + 641) >> 5 at 100 mV above 500 mV.
            Self::Axp2101Bldo1 | Self::Axp2101Dldo1 => 500 + ((level + 641) >> 5) * 100,
        }
    }
}

/// Failure of a PMIC or expander register operation.
#[derive(Debug)]
pub enum PmicControlError<BusError> {
    /// The AXP192 rejected an operation.
    Axp192(Axp192Error<BusError>),
    /// The AXP2101 rejected an operation.
    Axp2101(Axp2101Error<BusError>),
    /// The AW9523B rejected an operation.
    Aw9523(Aw9523Error<BusError>),
}

/// Backlight dimmed by programming its PMIC regulator.
pub struct PmicBacklight<I2C> {
    i2c: I2C,
    rail: BacklightRail,
}

impl<I2C: I2c> PmicBacklight<I2C> {
    /// Returns the regulator that feeds the backlight.
    #[must_use]
    pub const fn rail(&self) -> BacklightRail {
        self.rail
    }

    fn apply(&mut self, brightness: u8) -> Result<(), PmicControlError<I2C::Error>> {
        let on = brightness != 0;
        let millivolts = self.rail.millivolts(brightness);
        match self.rail {
            BacklightRail::Axp192Dcdc3 | BacklightRail::Axp192Ldo2 | BacklightRail::Axp192Ldo3 => {
                let rail = match self.rail {
                    BacklightRail::Axp192Dcdc3 => Rail::Dcdc3,
                    BacklightRail::Axp192Ldo2 => Rail::Ldo2,
                    _ => Rail::Ldo3,
                };
                let mut pmic =
                    Axp192::new(&mut self.i2c, PMIC_ADDRESS).map_err(PmicControlError::Axp192)?;
                if on {
                    pmic.set_millivolts(rail, millivolts)
                        .map_err(PmicControlError::Axp192)?;
                }
                pmic.set_enabled(rail, on).map_err(PmicControlError::Axp192)
            }
            BacklightRail::Axp2101Bldo1 | BacklightRail::Axp2101Dldo1 => {
                let ldo = if self.rail == BacklightRail::Axp2101Bldo1 {
                    Ldo::Bldo1
                } else {
                    Ldo::Dldo1
                };
                let mut pmic =
                    Axp2101::new(&mut self.i2c, PMIC_ADDRESS).map_err(PmicControlError::Axp2101)?;
                if on {
                    pmic.set_millivolts(ldo, millivolts)
                        .map_err(PmicControlError::Axp2101)?;
                }
                pmic.set_enabled(ldo, on).map_err(PmicControlError::Axp2101)
            }
        }
    }
}

impl<I2C: I2c> PanelBacklight for PmicBacklight<I2C> {
    type Error = PmicControlError<I2C::Error>;

    fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::Error> {
        self.apply(brightness)
    }
}

/// Power layout and panel options used by the implementation factory.
pub struct PmicDisplayConfig<M> {
    layout: PowerLayout,
    panel: MipiDbiDriverConfig<M>,
}

impl<M> PmicDisplayConfig<M> {
    /// Combines the Board's power layout with its panel controller options.
    #[must_use]
    pub const fn new(layout: PowerLayout, panel: MipiDbiDriverConfig<M>) -> Self {
        Self { layout, panel }
    }
}

/// Concrete resources consumed during initialization.
pub struct PmicDisplayBindings<SPI, DC, RST, I2C, DELAY, const BUFFER_SIZE: usize> {
    spi: SPI,
    dc: DC,
    reset: Option<RST>,
    i2c: I2C,
    delay: DELAY,
    buffer: &'static mut [u8; BUFFER_SIZE],
}

impl<SPI, DC, RST, I2C, DELAY, const BUFFER_SIZE: usize>
    PmicDisplayBindings<SPI, DC, RST, I2C, DELAY, BUFFER_SIZE>
{
    /// Combines the panel's SPI device and D/C output, its GPIO reset when the
    /// layout has one, the PMIC control bus, a delay source, and a statically
    /// owned transfer buffer.
    #[must_use]
    pub const fn new(
        spi: SPI,
        dc: DC,
        reset: Option<RST>,
        i2c: I2C,
        delay: DELAY,
        buffer: &'static mut [u8; BUFFER_SIZE],
    ) -> Self {
        Self {
            spi,
            dc,
            reset,
            i2c,
            delay,
            buffer,
        }
    }
}

/// Display initialization failure.
#[derive(Debug)]
pub enum PmicDisplayInitError<BusError, InterfaceError, ResetError> {
    /// The PMIC at 34h is not the one the power layout uses, or no supported
    /// PMIC answered with its ID.
    UnsupportedPmic,
    /// The CoreS3 AW9523B did not answer with its ID.
    MissingExpander,
    /// The Board supplied a GPIO reset to a layout whose reset is behind a
    /// helper chip, or omitted the GPIO reset its layout needs.
    ResetWiring,
    /// A PMIC or expander register operation failed.
    Power(PmicControlError<BusError>),
    /// Controller initialization or the first backlight write failed.
    Panel(MipiDbiInitError<InterfaceError, ResetError, PmicControlError<BusError>>),
}

/// Initialized panel implementing Barracuda's stable display API.
pub type PmicDisplay<SPI, DC, M, RST, I2C, DELAY> =
    MipiDbiDisplay<SpiInterface<'static, SPI, DC>, M, PanelReset<RST>, PmicBacklight<I2C>, DELAY>;

/// Static factory used by generated Board composition.
pub struct M5StackPmicDisplayImplementation<SPI, DC, M, RST, I2C, DELAY, const BUFFER_SIZE: usize>(
    PhantomData<SPI>,
    PhantomData<DC>,
    PhantomData<M>,
    PhantomData<RST>,
    PhantomData<I2C>,
    PhantomData<DELAY>,
);

impl<SPI, DC, M, RST, I2C, DELAY, const BUFFER_SIZE: usize> PeripheralImplementation
    for M5StackPmicDisplayImplementation<SPI, DC, M, RST, I2C, DELAY, BUFFER_SIZE>
where
    SPI: SpiDevice + 'static,
    DC: OutputPin + 'static,
    M: Model + 'static,
    M::ColorFormat: InterfacePixelFormat<u8>,
    RST: OutputPin + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = PmicDisplayBindings<SPI, DC, RST, I2C, DELAY, BUFFER_SIZE>;
    type Config = PmicDisplayConfig<M>;
    type Peripheral = PmicDisplay<SPI, DC, M, RST, I2C, DELAY>;
    type Error = PmicDisplayInitError<I2C::Error, SpiError<SPI::Error, DC::Error>, RST::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let PmicDisplayBindings {
            spi,
            dc,
            reset,
            mut i2c,
            mut delay,
            buffer,
        } = bindings;
        if config.layout.has_gpio_reset() != reset.is_some() {
            return Err(PmicDisplayInitError::ResetWiring);
        }
        let rail = power_up(&mut i2c, config.layout, &mut delay)?;
        // The helper chip already reset the panel when the layout has no GPIO
        // reset: an always-present line that does nothing then keeps
        // `initialize_on` from adding a DCS software reset.
        barracuda_mipi_dbi_display::initialize_on(
            SpiInterface::new(spi, dc, buffer),
            Some(PanelReset::new(reset, false)),
            Some(PmicBacklight { i2c, rail }),
            (),
            delay,
            config.panel,
        )
        .map_err(PmicDisplayInitError::Panel)
    }
}

/// Which PMIC answers at 34h.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pmic {
    Axp192,
    Axp2101,
}

fn identify_pmic<I2C: I2c>(i2c: &mut I2C) -> Result<Option<Pmic>, PmicControlError<I2C::Error>> {
    // Both PMICs keep their ID in register 03h at 34h: 03h is an AXP192 and
    // 4Ah an AXP2101 (M5GFX autodetection, M5Unified `Power_Class::begin`).
    if Axp192::new(i2c, PMIC_ADDRESS)
        .and_then(|mut pmic| pmic.is_present())
        .map_err(PmicControlError::Axp192)?
    {
        return Ok(Some(Pmic::Axp192));
    }
    if Axp2101::new(i2c, PMIC_ADDRESS)
        .and_then(|mut pmic| pmic.is_present())
        .map_err(PmicControlError::Axp2101)?
    {
        return Ok(Some(Pmic::Axp2101));
    }
    Ok(None)
}

/// Switches the backlight off, powers the LCD, and resets it when the reset
/// line sits behind a helper chip. Returns the backlight regulator.
fn power_up<I2C, DELAY, I, R>(
    i2c: &mut I2C,
    layout: PowerLayout,
    delay: &mut DELAY,
) -> Result<BacklightRail, PmicDisplayInitError<I2C::Error, I, R>>
where
    I2C: I2c,
    DELAY: DelayNs,
{
    let pmic = identify_pmic(i2c).map_err(PmicDisplayInitError::Power)?;
    match (layout, pmic) {
        (PowerLayout::Core2, Some(Pmic::Axp192)) => {
            // M5GFX `reg_data_axp192_first`/`_reset`/`_second`: LDO2 = LCD
            // power at 3.3 V, GPIO4 = LCD reset, DC-DC3 = backlight.
            axp192_power_up(i2c, Rail::Dcdc3, Rail::Ldo2, 3300, true, delay)
                .map_err(PmicDisplayInitError::Power)?;
            Ok(BacklightRail::Axp192Dcdc3)
        }
        (PowerLayout::Core2, Some(Pmic::Axp2101)) => {
            // M5GFX Core2 v1.1: ALDO4 = TF/TP/LCD power, ALDO2 = LCD and touch
            // reset, BLDO1 = backlight. ALDO4 at 3.3 V as in Espressif's
            // m5stack_core_2 power manager.
            axp2101_core2_power_up(i2c, delay).map_err(PmicDisplayInitError::Power)?;
            Ok(BacklightRail::Axp2101Bldo1)
        }
        (PowerLayout::Tough, Some(Pmic::Axp192)) => {
            // M5Unified: LDO2 3.3 V = LCD power; M5GFX: GPIO4 = LCD reset,
            // LDO3 = backlight (Light_M5Tough).
            axp192_power_up(i2c, Rail::Ldo3, Rail::Ldo2, 3300, true, delay)
                .map_err(PmicDisplayInitError::Power)?;
            Ok(BacklightRail::Axp192Ldo3)
        }
        (PowerLayout::StickC, Some(Pmic::Axp192)) => {
            // M5Unified: LDO3 3.0 V = LCD power; M5GFX: LDO2 = backlight
            // (Light_M5StickC).
            axp192_power_up(i2c, Rail::Ldo2, Rail::Ldo3, 3000, false, delay)
                .map_err(PmicDisplayInitError::Power)?;
            Ok(BacklightRail::Axp192Ldo2)
        }
        (PowerLayout::Station, Some(Pmic::Axp192)) => {
            // M5Unified: LDO2 3.3 V; M5GFX: LDO3 = backlight, as on the Tough.
            axp192_power_up(i2c, Rail::Ldo3, Rail::Ldo2, 3300, false, delay)
                .map_err(PmicDisplayInitError::Power)?;
            Ok(BacklightRail::Axp192Ldo3)
        }
        (PowerLayout::CoreS3, Some(Pmic::Axp2101)) => {
            cores3_power_up(i2c, delay)?;
            Ok(BacklightRail::Axp2101Dldo1)
        }
        _ => Err(PmicDisplayInitError::UnsupportedPmic),
    }
}

fn axp192_power_up<I2C: I2c, DELAY: DelayNs>(
    i2c: &mut I2C,
    backlight: Rail,
    lcd_power: Rail,
    lcd_millivolts: u16,
    gpio4_reset: bool,
    delay: &mut DELAY,
) -> Result<(), PmicControlError<I2C::Error>> {
    let mut pmic = Axp192::new(i2c, PMIC_ADDRESS).map_err(PmicControlError::Axp192)?;
    pmic.set_enabled(backlight, false)
        .map_err(PmicControlError::Axp192)?;
    pmic.set_millivolts(lcd_power, lcd_millivolts)
        .map_err(PmicControlError::Axp192)?;
    pmic.set_enabled(lcd_power, true)
        .map_err(PmicControlError::Axp192)?;
    delay.delay_ms(POWER_SETTLE_MS);
    if gpio4_reset {
        pmic.pulse_reset_low(Axp192Gpio::Gpio4, RESET_LOW_MS, RESET_SETTLE_MS, delay)
            .map_err(PmicControlError::Axp192)?;
    }
    Ok(())
}

fn axp2101_core2_power_up<I2C: I2c, DELAY: DelayNs>(
    i2c: &mut I2C,
    delay: &mut DELAY,
) -> Result<(), PmicControlError<I2C::Error>> {
    let mut pmic = Axp2101::new(i2c, PMIC_ADDRESS).map_err(PmicControlError::Axp2101)?;
    pmic.set_enabled(Ldo::Bldo1, false)
        .map_err(PmicControlError::Axp2101)?;
    pmic.set_millivolts(Ldo::Aldo4, 3300)
        .map_err(PmicControlError::Axp2101)?;
    pmic.set_enabled(Ldo::Aldo4, true)
        .map_err(PmicControlError::Axp2101)?;
    delay.delay_ms(POWER_SETTLE_MS);
    pmic.pulse_off(Ldo::Aldo2, RESET_LOW_MS, RESET_SETTLE_MS, delay)
        .map_err(PmicControlError::Axp2101)
}

fn cores3_power_up<I2C, DELAY, I, R>(
    i2c: &mut I2C,
    delay: &mut DELAY,
) -> Result<(), PmicDisplayInitError<I2C::Error, I, R>>
where
    I2C: I2c,
    DELAY: DelayNs,
{
    // The LCD logic runs from the 3.3 V system rail; only its backlight,
    // DLDO1, is switched (M5GFX Light_M5StackCoreS3).
    Axp2101::new(i2c, PMIC_ADDRESS)
        .and_then(|mut pmic| pmic.set_enabled(Ldo::Dldo1, false))
        .map_err(|error| PmicDisplayInitError::Power(PmicControlError::Axp2101(error)))?;
    let mut expander = Aw9523::new(i2c, EXPANDER_ADDRESS)
        .map_err(|error| PmicDisplayInitError::Power(PmicControlError::Aw9523(error)))?;
    // M5GFX checks AW9523B register 10h for 23h before trusting the board.
    if !expander
        .is_present()
        .map_err(|error| PmicDisplayInitError::Power(PmicControlError::Aw9523(error)))?
    {
        return Err(PmicDisplayInitError::MissingExpander);
    }
    expander
        .pulse_reset_low(CORES3_LCD_RESET_PIN, RESET_LOW_MS, RESET_SETTLE_MS, delay)
        .map_err(|error| PmicDisplayInitError::Power(PmicControlError::Aw9523(error)))
}

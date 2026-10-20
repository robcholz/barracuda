//! M5Stack Chain Captain display implementation for Barracuda's display API.
//!
//! The Chain Captain's 1.54-inch 240 x 240 HSD015BPN2-A0 panel is driven by a
//! Jadard JD9853 on a write-only SPI link. Its supply, reset, and PWM
//! backlight are pins of the M5IOE1 expander on the internal I2C bus, which
//! stays private to this implementation. Everything follows M5Stack's M5GFX
//! (`board_M5ChainCaptain`, `Light_M5ChainCaptain`):
//!
//! - `IO12` enables the panel supply and `IO1` is its active-low reset;
//! - `IO11` carries PWM channel 3 at 1 kHz as the backlight, with a gamma-2
//!   duty curve;
//! - the panel is mounted upside down in the JD9853's 240 x 320 GRAM, so the
//!   upright image is the controller's half turn, 80 rows into GRAM;
//! - pixels are RGB ordered and not inverted.

#![no_std]

use core::{convert::Infallible, marker::PhantomData};

use barracuda_driver_jd9853::{GRAM_HEIGHT, GRAM_WIDTH, HSD015BPN2_INIT};
use barracuda_driver_m5ioe1::{Error as ExpanderError, M5ioe1, MAX_DUTY, PwmChannel};
use barracuda_peripheral::{
    PeripheralImplementation,
    display::{
        Display, DisplayDescriptor, DisplayFeatures, DisplayOrientation, DisplayPower,
        DisplayTechnology, PixelFormat, RefreshRequest,
    },
};
use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::Rgb565,
    primitives::Rectangle,
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c, spi::SpiDevice};
use mipidsi::{
    Builder, Display as MipidsiDisplay, InitError, NoResetPin,
    dcs::{InterfaceExt, SetAddressMode, SetInvertMode},
    interface::{Interface, SpiError, SpiInterface},
    models::{Model, ModelInitError},
    options::{ColorInversion, ColorOrder, ModelOptions, Orientation, Rotation},
};

/// Visible panel side in pixels.
pub const SIDE: u16 = 240;

/// M5IOE1 pins and PWM settings from M5GFX.
const PANEL_SUPPLY: u8 = 12;
const PANEL_RESET: u8 = 1;
const BACKLIGHT_CHANNEL: PwmChannel = PwmChannel::Ch3;
const BACKLIGHT_PWM_HZ: u16 = 1_000;
const RESET_LOW_MS: u32 = 10;
const RESET_RECOVERY_MS: u32 = 20;
/// Full brightness on the portable `0..=255` scale.
const FULL_BRIGHTNESS: u8 = u8::MAX;

/// Takes the statically allocated transfer buffer used by the one panel.
#[must_use]
pub fn take_transfer_buffer() -> &'static mut [u8; 512] {
    static BUFFER: static_cell::StaticCell<[u8; 512]> = static_cell::StaticCell::new();
    BUFFER.init([0; 512])
}

/// JD9853 model for `mipidsi`, bringing the panel up with M5GFX's sequence.
pub struct Jd9853;

impl Model for Jd9853 {
    type ColorFormat = Rgb565;
    const FRAMEBUFFER_SIZE: (u16, u16) = (GRAM_WIDTH, GRAM_HEIGHT);

    fn init<DELAY, DI>(
        &mut self,
        di: &mut DI,
        delay: &mut DELAY,
        options: &ModelOptions,
    ) -> Result<SetAddressMode, ModelInitError<DI::Error>>
    where
        DELAY: DelayNs,
        DI: Interface,
    {
        for command in HSD015BPN2_INIT {
            di.write_raw(command.command, command.data)?;
            if command.delay_ms != 0 {
                delay.delay_ms(u32::from(command.delay_ms));
            }
        }
        let address_mode = SetAddressMode::from(options);
        di.write_command(address_mode)?;
        di.write_command(SetInvertMode::new(options.invert_colors))?;
        Ok(address_mode)
    }
}

/// Maps the portable brightness to the 12-bit duty with M5GFX's gamma-2
/// curve.
#[must_use]
pub fn backlight_duty(brightness: u8) -> u16 {
    let squared = u32::from(brightness) * u32::from(brightness);
    let duty = (squared * u32::from(MAX_DUTY) + 32_512) / 65_025;
    u16::try_from(duty).unwrap_or(MAX_DUTY)
}

/// Board options of the Chain Captain display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainCaptainDisplayConfig {
    expander_address: u8,
    orientation: DisplayOrientation,
}

impl ChainCaptainDisplayConfig {
    /// Creates configuration from the M5IOE1's I2C address and the initial
    /// orientation relative to the upright panel.
    #[must_use]
    pub const fn new(expander_address: u8, orientation: DisplayOrientation) -> Self {
        Self {
            expander_address,
            orientation,
        }
    }
}

/// Move-only resources consumed by initialization.
pub struct ChainCaptainDisplayBindings<SPI, DC, I2C, DELAY> {
    spi: SPI,
    dc: DC,
    i2c: I2C,
    delay: DELAY,
    buffer: &'static mut [u8; 512],
}

impl<SPI, DC, I2C, DELAY> ChainCaptainDisplayBindings<SPI, DC, I2C, DELAY> {
    /// Combines the panel's SPI device and data/command output with the
    /// expander's bus, a delay, and the transfer buffer.
    #[must_use]
    pub const fn new(
        spi: SPI,
        dc: DC,
        i2c: I2C,
        delay: DELAY,
        buffer: &'static mut [u8; 512],
    ) -> Self {
        Self {
            spi,
            dc,
            i2c,
            delay,
            buffer,
        }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum ChainCaptainInitError<SpiE, DcE, I2cE> {
    /// The M5IOE1 could not be woken or its pins configured.
    Expander(ExpanderError<I2cE>),
    /// Controller initialization failed.
    Display(InitError<SpiError<SpiE, DcE>, Infallible>),
}

/// Runtime display operation failure.
#[derive(Debug)]
pub enum ChainCaptainDisplayError<SpiE, DcE, I2cE> {
    /// The panel interface rejected a command.
    Interface(SpiError<SpiE, DcE>),
    /// The backlight PWM could not be set.
    Expander(ExpanderError<I2cE>),
}

type PanelInterface<SPI, DC> = SpiInterface<'static, SPI, DC>;

/// [`ChainCaptainDisplayError`] for one set of bindings.
pub type ControlErrorFor<SPI, DC, I2C> = ChainCaptainDisplayError<
    <SPI as embedded_hal::spi::ErrorType>::Error,
    <DC as embedded_hal::digital::ErrorType>::Error,
    <I2C as embedded_hal::i2c::ErrorType>::Error,
>;

/// Initialized Chain Captain display.
pub struct ChainCaptainDisplay<SPI, DC, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
{
    inner: MipidsiDisplay<PanelInterface<SPI, DC>, Jd9853, NoResetPin>,
    expander: M5ioe1<I2C>,
    delay: DELAY,
    brightness: u8,
}

/// Bindings carried only at the type level by the static factory.
type FactoryTypes<SPI, DC, I2C, DELAY> = PhantomData<fn() -> (SPI, DC, I2C, DELAY)>;

/// Static factory used by generated Board composition.
pub struct ChainCaptainDisplayImplementation<SPI, DC, I2C, DELAY>(
    FactoryTypes<SPI, DC, I2C, DELAY>,
);

/// [`ChainCaptainInitError`] for one set of bindings.
pub type InitErrorFor<SPI, DC, I2C> = ChainCaptainInitError<
    <SPI as embedded_hal::spi::ErrorType>::Error,
    <DC as embedded_hal::digital::ErrorType>::Error,
    <I2C as embedded_hal::i2c::ErrorType>::Error,
>;

impl<SPI, DC, I2C, DELAY> PeripheralImplementation
    for ChainCaptainDisplayImplementation<SPI, DC, I2C, DELAY>
where
    SPI: SpiDevice + 'static,
    DC: OutputPin + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = ChainCaptainDisplayBindings<SPI, DC, I2C, DELAY>;
    type Config = ChainCaptainDisplayConfig;
    type Peripheral = ChainCaptainDisplay<SPI, DC, I2C, DELAY>;
    type Error = InitErrorFor<SPI, DC, I2C>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let ChainCaptainDisplayBindings {
            spi,
            dc,
            i2c,
            mut delay,
            buffer,
        } = bindings;
        let mut expander =
            M5ioe1::new(i2c, config.expander_address).map_err(ChainCaptainInitError::Expander)?;
        power_up(&mut expander, &mut delay).map_err(ChainCaptainInitError::Expander)?;
        let inner = Builder::new(Jd9853, SpiInterface::new(spi, dc, buffer))
            .display_size(SIDE, SIDE)
            .display_offset(0, 0)
            .color_order(ColorOrder::Rgb)
            .invert_colors(ColorInversion::Normal)
            .orientation(mipidsi_orientation(config.orientation))
            .init(&mut delay)
            .map_err(ChainCaptainInitError::Display)?;
        expander
            .set_pwm_duty(
                BACKLIGHT_CHANNEL,
                backlight_duty(FULL_BRIGHTNESS),
                false,
                true,
            )
            .map_err(ChainCaptainInitError::Expander)?;
        Ok(ChainCaptainDisplay {
            inner,
            expander,
            delay,
            brightness: FULL_BRIGHTNESS,
        })
    }
}

/// Wakes the expander, powers and resets the panel, and prepares the dark
/// backlight PWM.
fn power_up<I2C: I2c, DELAY: DelayNs>(
    expander: &mut M5ioe1<I2C>,
    delay: &mut DELAY,
) -> Result<(), ExpanderError<I2C::Error>> {
    expander.wake()?;
    expander.configure_output(PANEL_SUPPLY, true)?;
    expander.configure_output(PANEL_RESET, false)?;
    delay.delay_ms(RESET_LOW_MS);
    expander.set_output(PANEL_RESET, true)?;
    delay.delay_ms(RESET_RECOVERY_MS);
    expander.configure_output(BACKLIGHT_CHANNEL.pin(), false)?;
    expander.set_pwm_frequency(BACKLIGHT_PWM_HZ)?;
    expander.set_pwm_duty(BACKLIGHT_CHANNEL, 0, false, true)
}

const fn mipidsi_orientation(orientation: DisplayOrientation) -> Orientation {
    let rotation = match orientation {
        DisplayOrientation::Deg0 => Rotation::Deg0,
        DisplayOrientation::Deg90 => Rotation::Deg90,
        DisplayOrientation::Deg180 => Rotation::Deg180,
        DisplayOrientation::Deg270 => Rotation::Deg270,
    };
    // M5GFX `offset_rotation = 2`: the panel is mounted upside down.
    Orientation::new().rotate(Rotation::Deg180).rotate(rotation)
}

impl<SPI, DC, I2C, DELAY> ChainCaptainDisplay<SPI, DC, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    I2C: I2c,
{
    fn set_backlight(&mut self, brightness: u8) -> Result<(), ControlErrorFor<SPI, DC, I2C>> {
        self.expander
            .set_pwm_duty(BACKLIGHT_CHANNEL, backlight_duty(brightness), false, true)
            .map_err(ChainCaptainDisplayError::Expander)
    }
}

impl<SPI, DC, I2C, DELAY> OriginDimensions for ChainCaptainDisplay<SPI, DC, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
{
    fn size(&self) -> Size {
        Size::new(u32::from(SIDE), u32::from(SIDE))
    }
}

impl<SPI, DC, I2C, DELAY> DrawTarget for ChainCaptainDisplay<SPI, DC, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
{
    type Color = Rgb565;
    type Error = SpiError<SPI::Error, DC::Error>;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        self.inner.draw_iter(pixels)
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        self.inner.fill_contiguous(area, colors)
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        self.inner.fill_solid(area, color)
    }
}

impl<SPI, DC, I2C, DELAY> Display for ChainCaptainDisplay<SPI, DC, I2C, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    I2C: I2c,
    DELAY: DelayNs,
{
    type ControlError = ChainCaptainDisplayError<SPI::Error, DC::Error, I2C::Error>;
    type RenderError = SpiError<SPI::Error, DC::Error>;

    fn descriptor(&self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Rgb565,
            DisplayTechnology::Lcd,
            DisplayFeatures::BRIGHTNESS | DisplayFeatures::ORIENTATION | DisplayFeatures::SLEEP,
        )
    }

    fn draw_rgb565(&mut self, area: Rectangle, pixels: &[Rgb565]) -> Result<(), Self::RenderError> {
        self.fill_contiguous(&area, pixels.iter().copied())
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        Ok(())
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        match power {
            DisplayPower::On => {
                self.inner
                    .wake(&mut self.delay)
                    .map_err(ChainCaptainDisplayError::Interface)?;
                self.set_backlight(self.brightness)
            }
            DisplayPower::Sleep | DisplayPower::Off => {
                self.set_backlight(0)?;
                self.inner
                    .sleep(&mut self.delay)
                    .map_err(ChainCaptainDisplayError::Interface)
            }
        }
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        self.set_backlight(brightness)?;
        self.brightness = brightness;
        Ok(())
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        self.inner
            .set_orientation(mipidsi_orientation(orientation))
            .map_err(ChainCaptainDisplayError::Interface)
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::backlight_duty;

    #[test]
    fn backlight_follows_m5gfx_gamma_curve() {
        assert_eq!(backlight_duty(0), 0);
        assert_eq!(backlight_duty(255), 4095);
        // (128^2 * 4095 + 32512) / 65025 = 1032.
        assert_eq!(backlight_duty(128), 1032);
        assert!(backlight_duty(1) <= 1);
    }
}

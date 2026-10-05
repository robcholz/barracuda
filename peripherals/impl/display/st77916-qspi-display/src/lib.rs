//! ST77916 QSPI display implementation for the Barracuda display API.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::QuadSpiBus;
use barracuda_driver_st77916::{
    COLOR_WRITE_OPCODE, COMMAND_WRITE_OPCODE, HEIGHT, INIT_COMMANDS, MEMORY_WRITE, WIDTH,
};
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
    geometry::{Dimensions, OriginDimensions, Size},
    pixelcolor::{Rgb565, Rgb888, RgbColor},
    primitives::Rectangle,
};
use embedded_hal::{delay::DelayNs, digital::OutputPin};

const CASET: u8 = 0x2a;
const RASET: u8 = 0x2b;
const MADCTL: u8 = 0x36;
const COLMOD: u8 = 0x3a;
const DISPLAY_OFF: u8 = 0x28;
const DISPLAY_ON: u8 = 0x29;
const SLEEP_IN: u8 = 0x10;
const SLEEP_OUT: u8 = 0x11;

/// Fixed native configuration of the ESP-VoCat round panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct St77916DisplayConfig;

impl St77916DisplayConfig {
    /// Creates the fixed 360-by-360 RGB565 panel configuration.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Move-only resources consumed by ST77916 initialization.
pub struct St77916DisplayBindings<BUS, DC, RESET, BACKLIGHT, POWER, DELAY> {
    bus: BUS,
    dc: DC,
    reset: RESET,
    backlight: BACKLIGHT,
    power_enable: POWER,
    delay: DELAY,
}

impl<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>
    St77916DisplayBindings<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>
{
    /// Combines the QSPI transport and Board control outputs.
    #[must_use]
    pub const fn new(
        bus: BUS,
        dc: DC,
        reset: RESET,
        backlight: BACKLIGHT,
        power_enable: POWER,
        delay: DELAY,
    ) -> Self {
        Self {
            bus,
            dc,
            reset,
            backlight,
            power_enable,
            delay,
        }
    }
}

/// ST77916 initialization, rendering, or control failure.
#[derive(Debug)]
pub enum St77916Error<BusError, DcError, ResetError, BacklightError, PowerError> {
    /// QSPI transport failed.
    Bus(BusError),
    /// Data/command output failed.
    Dc(DcError),
    /// Panel reset output failed.
    Reset(ResetError),
    /// Backlight output failed.
    Backlight(BacklightError),
    /// Shared LCD and microSD rail could not be enabled.
    Power(PowerError),
    /// A portable pixel region was outside the panel or had the wrong length.
    InvalidPixelRegion,
}

/// Initialized ST77916 panel.
pub struct St77916Display<BUS, DC, RESET, BACKLIGHT, POWER, DELAY> {
    bus: BUS,
    dc: DC,
    _reset: RESET,
    backlight: BACKLIGHT,
    _power_enable: POWER,
    delay: DELAY,
    orientation: DisplayOrientation,
}

/// [`St77916Error`] specialized to one concrete set of bindings.
type BindingError<BUS, DC, RESET, BACKLIGHT, POWER> = St77916Error<
    <BUS as QuadSpiBus>::Error,
    <DC as embedded_hal::digital::ErrorType>::Error,
    <RESET as embedded_hal::digital::ErrorType>::Error,
    <BACKLIGHT as embedded_hal::digital::ErrorType>::Error,
    <POWER as embedded_hal::digital::ErrorType>::Error,
>;

/// Bindings carried only at the type level by the static factory.
type Bindings<BUS, DC, RESET, BACKLIGHT, POWER, DELAY> =
    PhantomData<fn() -> (BUS, DC, RESET, BACKLIGHT, POWER, DELAY)>;

/// Static factory used by generated Board composition.
pub struct St77916DisplayImplementation<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>(
    Bindings<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>,
);

impl<BUS, DC, RESET, BACKLIGHT, POWER, DELAY> PeripheralImplementation
    for St77916DisplayImplementation<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>
where
    BUS: QuadSpiBus + 'static,
    DC: OutputPin + 'static,
    RESET: OutputPin + 'static,
    BACKLIGHT: OutputPin + 'static,
    POWER: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = St77916DisplayBindings<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>;
    type Config = St77916DisplayConfig;
    type Peripheral = St77916Display<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>;
    type Error = St77916Error<BUS::Error, DC::Error, RESET::Error, BACKLIGHT::Error, POWER::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        _config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        bindings
            .backlight
            .set_low()
            .map_err(St77916Error::Backlight)?;
        bindings
            .power_enable
            .set_low()
            .map_err(St77916Error::Power)?;
        bindings.delay.delay_ms(100);
        bindings.reset.set_high().map_err(St77916Error::Reset)?;
        bindings.delay.delay_ms(10);
        bindings.reset.set_low().map_err(St77916Error::Reset)?;
        bindings.delay.delay_ms(120);

        let mut display = St77916Display {
            bus: bindings.bus,
            dc: bindings.dc,
            _reset: bindings.reset,
            backlight: bindings.backlight,
            _power_enable: bindings.power_enable,
            delay: bindings.delay,
            orientation: DisplayOrientation::Deg0,
        };
        display.write_command(MADCTL, &[0x00])?;
        display.write_command(COLMOD, &[0x55])?;
        for command in INIT_COMMANDS {
            display.write_command(command.command, command.data)?;
            if command.delay_ms != 0 {
                display.delay.delay_ms(u32::from(command.delay_ms));
            }
        }
        display.write_command(DISPLAY_ON, &[])?;
        display
            .backlight
            .set_high()
            .map_err(St77916Error::Backlight)?;
        Ok(display)
    }
}

impl<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>
    St77916Display<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>
where
    BUS: QuadSpiBus,
    DC: OutputPin,
    RESET: OutputPin,
    BACKLIGHT: OutputPin,
    POWER: OutputPin,
{
    fn write_command(
        &mut self,
        command: u8,
        data: &[u8],
    ) -> Result<(), BindingError<BUS, DC, RESET, BACKLIGHT, POWER>> {
        self.dc.set_low().map_err(St77916Error::Dc)?;
        self.bus
            .write(COMMAND_WRITE_OPCODE, u32::from(command) << 8, data)
            .map_err(St77916Error::Bus)
    }

    fn write_pixels(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        pixels: &[u8],
    ) -> Result<(), BindingError<BUS, DC, RESET, BACKLIGHT, POWER>> {
        let x_end = x + width - 1;
        let y_end = y + height - 1;
        self.write_command(
            CASET,
            &[(x >> 8) as u8, x as u8, (x_end >> 8) as u8, x_end as u8],
        )?;
        self.write_command(
            RASET,
            &[(y >> 8) as u8, y as u8, (y_end >> 8) as u8, y_end as u8],
        )?;
        self.dc.set_high().map_err(St77916Error::Dc)?;
        self.bus
            .write(COLOR_WRITE_OPCODE, u32::from(MEMORY_WRITE) << 8, pixels)
            .map_err(St77916Error::Bus)
    }
}

impl<BUS, DC, RESET, BACKLIGHT, POWER, DELAY> OriginDimensions
    for St77916Display<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>
{
    fn size(&self) -> Size {
        Size::new(u32::from(WIDTH), u32::from(HEIGHT))
    }
}

impl<BUS, DC, RESET, BACKLIGHT, POWER, DELAY> DrawTarget
    for St77916Display<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>
where
    BUS: QuadSpiBus,
    DC: OutputPin,
    RESET: OutputPin,
    BACKLIGHT: OutputPin,
    POWER: OutputPin,
{
    type Color = Rgb565;
    type Error = St77916Error<BUS::Error, DC::Error, RESET::Error, BACKLIGHT::Error, POWER::Error>;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if point.x < 0
                || point.y < 0
                || point.x >= i32::from(WIDTH)
                || point.y >= i32::from(HEIGHT)
            {
                continue;
            }
            let value = rgb565_components(color.r(), color.g(), color.b()).to_be_bytes();
            self.write_pixels(point.x as u16, point.y as u16, 1, 1, &value)?;
        }
        Ok(())
    }
}

impl<BUS, DC, RESET, BACKLIGHT, POWER, DELAY> Display
    for St77916Display<BUS, DC, RESET, BACKLIGHT, POWER, DELAY>
where
    BUS: QuadSpiBus,
    DC: OutputPin,
    RESET: OutputPin,
    BACKLIGHT: OutputPin,
    POWER: OutputPin,
    DELAY: DelayNs,
{
    type ControlError =
        St77916Error<BUS::Error, DC::Error, RESET::Error, BACKLIGHT::Error, POWER::Error>;
    type RenderError = Self::ControlError;

    fn descriptor(&self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Rgb565,
            DisplayTechnology::Lcd,
            DisplayFeatures::BRIGHTNESS | DisplayFeatures::ORIENTATION | DisplayFeatures::SLEEP,
        )
    }

    fn draw_rgb888(&mut self, area: Rectangle, pixels: &[Rgb888]) -> Result<(), Self::RenderError> {
        if area.intersection(&self.bounding_box()) != area
            || area.size.width == 0
            || area.size.height == 0
        {
            return Err(St77916Error::InvalidPixelRegion);
        }
        let width =
            usize::try_from(area.size.width).map_err(|_| St77916Error::InvalidPixelRegion)?;
        let expected = width
            .checked_mul(
                usize::try_from(area.size.height).map_err(|_| St77916Error::InvalidPixelRegion)?,
            )
            .ok_or(St77916Error::InvalidPixelRegion)?;
        if pixels.len() != expected {
            return Err(St77916Error::InvalidPixelRegion);
        }

        let mut row = [0u8; WIDTH as usize * 2];
        for (line, source) in pixels.chunks_exact(width).enumerate() {
            for (target, color) in row[..width * 2].chunks_exact_mut(2).zip(source) {
                target.copy_from_slice(&rgb565(*color).to_be_bytes());
            }
            self.write_pixels(
                area.top_left.x as u16,
                area.top_left.y as u16 + line as u16,
                area.size.width as u16,
                1,
                &row[..width * 2],
            )?;
        }
        Ok(())
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        Ok(())
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        match power {
            DisplayPower::On => {
                self.write_command(SLEEP_OUT, &[])?;
                self.delay.delay_ms(120);
                self.write_command(DISPLAY_ON, &[])?;
                self.backlight.set_high().map_err(St77916Error::Backlight)
            }
            DisplayPower::Sleep | DisplayPower::Off => {
                self.backlight.set_low().map_err(St77916Error::Backlight)?;
                self.write_command(DISPLAY_OFF, &[])?;
                self.write_command(SLEEP_IN, &[])
            }
        }
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        if brightness == 0 {
            self.backlight.set_low().map_err(St77916Error::Backlight)
        } else {
            self.backlight.set_high().map_err(St77916Error::Backlight)
        }
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        let madctl = match orientation {
            DisplayOrientation::Deg0 => 0x00,
            DisplayOrientation::Deg90 => 0x60,
            DisplayOrientation::Deg180 => 0xc0,
            DisplayOrientation::Deg270 => 0xa0,
        };
        self.write_command(MADCTL, &[madctl])?;
        self.orientation = orientation;
        Ok(())
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(())
    }
}

fn rgb565(color: Rgb888) -> u16 {
    rgb565_components(color.r(), color.g(), color.b())
}

fn rgb565_components(red: u8, green: u8, blue: u8) -> u16 {
    (u16::from(red & 0xf8) << 8) | (u16::from(green & 0xfc) << 3) | (u16::from(blue) >> 3)
}

//! MIPI DSI display implementation for the M5Stack CoreP4X's 2.64-inch
//! 480x480 ST7102 panel.
//!
//! The bring-up follows M5GFX's CoreP4X autodetect branch (`M5GFX.cpp`,
//! `board_M5CoreP4X`) and its `Light_M5CoreP4X` backlight: the M5PM1 enables
//! the 5 V boost converter, the M5IOE1 expander powers the panel, releases its
//! reset, and dims its backlight with PWM on G9, and the ST7102 takes its
//! initialization sequence after video has started. Both helper chips stay
//! private to this implementation.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_m5ioe1::{Error as ExpanderError, M5ioe1, PwmChannel};
use barracuda_driver_m5pm1::{Error as PowerError, M5pm1, Rail};
use barracuda_driver_st712x::ST7102_INIT_COMMANDS;
use barracuda_peripheral::{
    PeripheralImplementation,
    display::{
        Display, DisplayDescriptor, DisplayFeatures, DisplayOrientation, DisplayPower,
        DisplayTechnology, DsiHost, DsiPanelConfig, DsiVideoTiming, PixelFormat, RefreshRequest,
    },
};
use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{Dimensions, OriginDimensions, Size},
    pixelcolor::{IntoStorage, Rgb565},
    primitives::Rectangle,
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

const WIDTH: u16 = 480;
const HEIGHT: u16 = 480;

/// M5IOE1 pins M5GFX drives for the display: G8 touch reset, G9 backlight,
/// G10 panel power, G11 panel reset, and G12 the shared 3.3 V rail (M5-Bus,
/// microSD, IMU, infrared, Ethernet) that M5GFX enables in the same step.
const TOUCH_RESET: u8 = 8;
const BACKLIGHT: u8 = BACKLIGHT_CHANNEL.pin();
const PANEL_POWER: u8 = 10;
const PANEL_RESET: u8 = 11;
const SHARED_3V3: u8 = 12;
/// `Light_M5CoreP4X`: 1 kHz PWM on M5IOE1 PWM channel 1 (G9).
const BACKLIGHT_PWM_HZ: u16 = 1_000;
const BACKLIGHT_CHANNEL: PwmChannel = PwmChannel::Ch1;
/// M5GFX waits 150 ms after powering the panel before starting the link.
const POWER_SETTLE_MS: u32 = 150;
/// M5GFX waits 50 ms after the link comes up.
const LINK_SETTLE_MS: u32 = 50;
/// `Panel_ST7102::getResetDelayBeforeDpi`.
const RESET_BEFORE_VIDEO_MS: u32 = 120;

/// The link and video timing M5GFX gives the CoreP4X panel: two lanes at
/// 600 Mbps, a 24 MHz pixel clock, and its porches.
const PANEL: DsiPanelConfig = DsiPanelConfig {
    lane_bit_rate_mbps: 600,
    dpi_clock_mhz: 24,
    timing: DsiVideoTiming {
        width: WIDTH,
        height: HEIGHT,
        hsync_back_porch: 40,
        hsync_pulse_width: 2,
        hsync_front_porch: 40,
        vsync_back_porch: 8,
        vsync_pulse_width: 4,
        vsync_front_porch: 200,
    },
};

/// Board-owned addresses of the two helper chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreP4xDsiDisplayConfig {
    expander_address: u8,
    power_address: u8,
}

impl CoreP4xDsiDisplayConfig {
    /// Creates the configuration from the M5IOE1 and M5PM1 addresses.
    #[must_use]
    pub const fn new(expander_address: u8, power_address: u8) -> Self {
        Self {
            expander_address,
            power_address,
        }
    }
}

/// Move-only DSI host, control bus, and delay consumed by this implementation.
pub struct CoreP4xDsiDisplayBindings<HOST, I2C, DELAY> {
    host: HOST,
    i2c: I2C,
    delay: DELAY,
}

impl<HOST, I2C, DELAY> CoreP4xDsiDisplayBindings<HOST, I2C, DELAY> {
    /// Combines the DSI data plane with the helper chips' I2C bus.
    #[must_use]
    pub const fn new(host: HOST, i2c: I2C, delay: DELAY) -> Self {
        Self { host, i2c, delay }
    }
}

/// Display initialization failure.
#[derive(Debug)]
pub enum CoreP4xDsiDisplayInitError<BusError, HostError> {
    /// The M5IOE1 expander rejected a register write.
    Expander(ExpanderError<BusError>),
    /// The M5PM1 power controller rejected a register write.
    Power(PowerError<BusError>),
    /// The Platform DSI host rejected panel initialization.
    Host(HostError),
}

/// Runtime display operation failure.
#[derive(Debug)]
pub enum CoreP4xDsiDisplayError<BusError, HostError> {
    /// The Platform DSI host rejected the operation.
    Host(HostError),
    /// The backlight expander rejected a register write.
    Expander(ExpanderError<BusError>),
    /// The panel exposes only its native orientation.
    UnsupportedOrientation,
    /// Pixel data did not fill the requested rectangle.
    InvalidPixelCount,
}

/// Initialized CoreP4X display implementing Barracuda's display API.
pub struct CoreP4xDsiDisplay<HOST, I2C> {
    host: HOST,
    i2c: I2C,
    expander_address: u8,
    brightness: u8,
}

/// Static factory used by generated Board composition.
pub struct CoreP4xDsiDisplayImplementation<HOST, I2C, DELAY>(
    PhantomData<HOST>,
    PhantomData<I2C>,
    PhantomData<DELAY>,
);

impl<HOST, I2C, DELAY> PeripheralImplementation
    for CoreP4xDsiDisplayImplementation<HOST, I2C, DELAY>
where
    HOST: DsiHost + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = CoreP4xDsiDisplayBindings<HOST, I2C, DELAY>;
    type Config = CoreP4xDsiDisplayConfig;
    type Peripheral = CoreP4xDsiDisplay<HOST, I2C>;
    type Error = CoreP4xDsiDisplayInitError<I2C::Error, HOST::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        power_panel(&mut bindings.i2c, config).map_err(|error| match error {
            BringUpError::Expander(error) => CoreP4xDsiDisplayInitError::Expander(error),
            BringUpError::Power(error) => CoreP4xDsiDisplayInitError::Power(error),
        })?;
        bindings.delay.delay_ms(POWER_SETTLE_MS);

        let host = &mut bindings.host;
        host.initialize(PANEL)
            .map_err(CoreP4xDsiDisplayInitError::Host)?;
        bindings.delay.delay_ms(LINK_SETTLE_MS);
        // M5GFX's ST7102 takes its sequence in video mode
        // (`initInCommandMode() == false`).
        bindings.delay.delay_ms(RESET_BEFORE_VIDEO_MS);
        host.start_video()
            .map_err(CoreP4xDsiDisplayInitError::Host)?;
        for command in ST7102_INIT_COMMANDS {
            host.write_command(command.command, command.data)
                .map_err(CoreP4xDsiDisplayInitError::Host)?;
            if command.delay_ms != 0 {
                bindings.delay.delay_ms(u32::from(command.delay_ms));
            }
        }

        let mut display = CoreP4xDsiDisplay {
            host: bindings.host,
            i2c: bindings.i2c,
            expander_address: config.expander_address,
            brightness: u8::MAX,
        };
        display
            .apply_brightness(u8::MAX)
            .map_err(CoreP4xDsiDisplayInitError::Expander)?;
        Ok(display)
    }
}

enum BringUpError<BusError> {
    Expander(ExpanderError<BusError>),
    Power(PowerError<BusError>),
}

/// M5GFX's CoreP4X power-up: M5PM1 housekeeping and 5 V boost, then the
/// M5IOE1 pins as push-pull outputs driven high, and the backlight PWM at
/// zero duty until the panel is running.
fn power_panel<I2C: I2c>(
    i2c: &mut I2C,
    config: CoreP4xDsiDisplayConfig,
) -> Result<(), BringUpError<I2C::Error>> {
    let mut power = M5pm1::new(i2c, config.power_address).map_err(BringUpError::Power)?;
    power.disable_i2c_sleep().map_err(BringUpError::Power)?;
    power.disable_watchdog().map_err(BringUpError::Power)?;
    power
        .set_rail_enabled(Rail::Boost, true)
        .map_err(BringUpError::Power)?;

    let mut expander = M5ioe1::new(i2c, config.expander_address).map_err(BringUpError::Expander)?;
    expander.wake().map_err(BringUpError::Expander)?;
    // M5GFX makes the pins push-pull outputs and then drives them high; the
    // driver latches the high level first so no pin glitches low.
    for pin in [TOUCH_RESET, BACKLIGHT, PANEL_POWER, PANEL_RESET, SHARED_3V3] {
        expander
            .configure_output(pin, true)
            .map_err(BringUpError::Expander)?;
    }
    expander
        .set_pwm_frequency(BACKLIGHT_PWM_HZ)
        .map_err(BringUpError::Expander)?;
    expander
        .set_pwm_duty(BACKLIGHT_CHANNEL, 0, false, true)
        .map_err(BringUpError::Expander)
}

/// `Light_M5CoreP4X::setBrightness`: the 8-bit level spread over the 12-bit
/// duty, `(level << 4) | (level >> 4)`.
const fn backlight_duty(brightness: u8) -> u16 {
    let level = brightness as u16;
    (level << 4) | (level >> 4)
}

/// M5GFX mounts the panel half a turn from its scan order
/// (`offset_rotation = 2`): a pixel at `(x, y)` is stored at
/// `(width - 1 - x, height - 1 - y)`.
const fn stored_origin(area: Rectangle) -> (u16, u16) {
    let right = area.top_left.x as u16 + area.size.width as u16;
    let bottom = area.top_left.y as u16 + area.size.height as u16;
    (WIDTH - right, HEIGHT - bottom)
}

impl<HOST, I2C> CoreP4xDsiDisplay<HOST, I2C>
where
    HOST: DsiHost,
    I2C: I2c,
{
    fn apply_brightness(&mut self, brightness: u8) -> Result<(), ExpanderError<I2C::Error>> {
        M5ioe1::new(&mut self.i2c, self.expander_address)?.set_pwm_duty(
            BACKLIGHT_CHANNEL,
            backlight_duty(brightness),
            false,
            true,
        )
    }

    /// Draws rows already bounded to the panel, rotated half a turn.
    fn draw_rows(
        &mut self,
        area: Rectangle,
        pixels: impl Iterator<Item = u16>,
    ) -> Result<(), CoreP4xDsiDisplayError<I2C::Error, HOST::Error>> {
        let width = area.size.width as usize;
        let (x, bottom) = stored_origin(area);
        let mut row = [0_u16; WIDTH as usize];
        let mut pixels = pixels;
        for line in 0..area.size.height as u16 {
            for target in row[..width].iter_mut().rev() {
                *target = pixels
                    .next()
                    .ok_or(CoreP4xDsiDisplayError::InvalidPixelCount)?;
            }
            let y = bottom + area.size.height as u16 - 1 - line;
            self.host
                .draw_rgb565(x, y, width as u16, 1, &row[..width])
                .map_err(CoreP4xDsiDisplayError::Host)?;
        }
        Ok(())
    }
}

impl<HOST: DsiHost, I2C: I2c> OriginDimensions for CoreP4xDsiDisplay<HOST, I2C> {
    fn size(&self) -> Size {
        Size::new(u32::from(WIDTH), u32::from(HEIGHT))
    }
}

impl<HOST: DsiHost, I2C: I2c> DrawTarget for CoreP4xDsiDisplay<HOST, I2C> {
    type Color = Rgb565;
    type Error = CoreP4xDsiDisplayError<I2C::Error, HOST::Error>;

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
            self.draw_rows(
                Rectangle::new(point, Size::new(1, 1)),
                core::iter::once(color.into_storage()),
            )?;
        }
        Ok(())
    }
}

impl<HOST: DsiHost, I2C: I2c> Display for CoreP4xDsiDisplay<HOST, I2C> {
    type ControlError = CoreP4xDsiDisplayError<I2C::Error, HOST::Error>;
    type RenderError = CoreP4xDsiDisplayError<I2C::Error, HOST::Error>;

    fn descriptor(&self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Rgb565,
            DisplayTechnology::Lcd,
            DisplayFeatures::BUFFERED | DisplayFeatures::BRIGHTNESS | DisplayFeatures::SLEEP,
        )
    }

    fn draw_rgb565(&mut self, area: Rectangle, pixels: &[Rgb565]) -> Result<(), Self::RenderError> {
        let clipped = area.intersection(&self.bounding_box());
        if clipped != area || area.size.width == 0 || area.size.height == 0 {
            return Err(CoreP4xDsiDisplayError::InvalidPixelCount);
        }
        let expected = (area.size.width as usize).saturating_mul(area.size.height as usize);
        if expected != pixels.len() {
            return Err(CoreP4xDsiDisplayError::InvalidPixelCount);
        }
        self.draw_rows(area, pixels.iter().map(|color| color.into_storage()))
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        self.host.present().map_err(CoreP4xDsiDisplayError::Host)
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        match power {
            DisplayPower::On => {
                self.host
                    .set_sleep(false)
                    .map_err(CoreP4xDsiDisplayError::Host)?;
                self.host
                    .set_display_enabled(true)
                    .map_err(CoreP4xDsiDisplayError::Host)?;
                self.apply_brightness(self.brightness)
                    .map_err(CoreP4xDsiDisplayError::Expander)
            }
            DisplayPower::Sleep => self
                .host
                .set_sleep(true)
                .map_err(CoreP4xDsiDisplayError::Host),
            DisplayPower::Off => {
                self.apply_brightness(0)
                    .map_err(CoreP4xDsiDisplayError::Expander)?;
                self.host
                    .set_display_enabled(false)
                    .map_err(CoreP4xDsiDisplayError::Host)
            }
        }
    }

    async fn set_brightness(&mut self, brightness: u8) -> Result<(), Self::ControlError> {
        self.brightness = brightness;
        self.apply_brightness(brightness)
            .map_err(CoreP4xDsiDisplayError::Expander)
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        if orientation == DisplayOrientation::Deg0 {
            Ok(())
        } else {
            Err(CoreP4xDsiDisplayError::UnsupportedOrientation)
        }
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests;

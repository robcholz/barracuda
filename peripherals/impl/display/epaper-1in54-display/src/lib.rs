//! Buffered 1.54-inch 200 x 200 e-paper implementation of Barracuda's stable
//! display peripheral.
//!
//! M5Stack fits two panel revisions to the same CoreInk and AirQ boards: Good
//! Display's GDEH0154D67 (Solomon SSD1681 controller) on early units and the
//! GDEW0154M09 (Fitipower JD79653A) on later ones. M5GFX tells them apart at
//! start-up and so does this implementation.
//!
//! M5GFX reads ID registers (`0x2F` on the SSD1681, `0x70` on the JD79653A)
//! back over the bidirectional data line while chip-select stays low across a
//! data/command change. An `embedded-hal` [`SpiDevice`] cannot keep
//! chip-select asserted while the separate D/C line changes, so this
//! implementation uses the controllers' opposite `BUSY` polarity instead:
//! once a hardware reset has completed, the SSD1681 holds `BUSY` low when idle
//! (busy is high), while the JD79653A holds `BUSY_N` high when idle (busy is
//! low). The line is sampled repeatedly after the reset and only a stable level
//! selects a controller.
//!
//! The SSD1681 path uses `epd-waveshare`'s `epd1in54_v2` driver; the JD79653A
//! path uses M5GFX's initialization, LUTs, and refresh sequence through
//! `barracuda-driver-jd79653a`.

#![no_std]

use core::{convert::Infallible, marker::PhantomData};

use barracuda_driver_jd79653a::{self as jd79653a, FrameRam, Jd79653a, Window};
use barracuda_peripheral::{
    PeripheralImplementation,
    display::{
        Display, DisplayDescriptor, DisplayFeatures, DisplayOrientation, DisplayPower,
        DisplayTechnology, PixelFormat, RefreshMode, RefreshRequest,
    },
};
use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Point, Size},
    pixelcolor::{Rgb565, RgbColor},
    primitives::Rectangle,
};
use embedded_hal::{
    delay::DelayNs,
    digital::{InputPin, OutputPin},
    spi::SpiDevice,
};
use epd_waveshare::{
    color::Color,
    epd1in54_v2::{Display1in54, Epd1in54, HEIGHT, WIDTH},
    prelude::{RefreshLut, WaveshareDisplay},
};

/// Stable descriptor shared by both panel revisions.
pub const DESCRIPTOR: DisplayDescriptor = DisplayDescriptor::new(
    Size::new(WIDTH, HEIGHT),
    PixelFormat::Binary,
    DisplayTechnology::Epaper,
    DisplayFeatures::BUFFERED
        .union(DisplayFeatures::PARTIAL_REFRESH)
        .union(DisplayFeatures::BUSY)
        .union(DisplayFeatures::SLEEP),
);

/// Bytes in one packed frame.
const FRAME_BYTES: usize = jd79653a::ROW_BYTES * jd79653a::HEIGHT as usize;
/// Reset pulse and recovery used by M5GFX `_pin_reset` (2 ms low, 10 ms high).
const RESET_LOW_MS: u32 = 2;
const RESET_RECOVERY_MS: u32 = 10;
/// Settle time before `BUSY` is sampled for detection. Both controllers finish
/// their reset well within it (vendor sample code waits 10 to 20 ms).
const DETECT_SETTLE_MS: u32 = 100;
/// Consecutive identical samples, 5 ms apart, that select a controller.
const DETECT_SAMPLES: u32 = 4;
const DETECT_SAMPLE_INTERVAL_MS: u32 = 5;
/// Samples taken before detection gives up on an unsettled line.
const DETECT_MAX_SAMPLES: u32 = 200;
/// Minimum time between a JD79653A refresh and the next transfer (M5GFX
/// `Panel_GDEW0154M09::_refresh_msec`).
const JD79653A_REFRESH_MS: u32 = 320;

/// Panel controller identified at start-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelController {
    /// GDEH0154D67 panel with a Solomon SSD1681 controller.
    Ssd1681,
    /// GDEW0154M09 panel with a Fitipower JD79653A controller.
    Jd79653a,
}

impl PanelController {
    /// Maps a settled, idle `BUSY` level to its controller.
    #[must_use]
    pub const fn from_idle_busy_level(high: bool) -> Self {
        if high { Self::Jd79653a } else { Self::Ssd1681 }
    }
}

/// Move-only resources consumed by the implementation.
pub struct Epaper1in54Bindings<SPI, BUSY, DC, RST, POWER, DELAY> {
    spi: SPI,
    busy: BUSY,
    dc: DC,
    reset: RST,
    power_hold: POWER,
    delay: DELAY,
}

impl<SPI, BUSY, DC, RST, POWER, DELAY> Epaper1in54Bindings<SPI, BUSY, DC, RST, POWER, DELAY> {
    /// Combines the selected SPI device and panel control signals.
    #[must_use]
    pub const fn new(
        spi: SPI,
        busy: BUSY,
        dc: DC,
        reset: RST,
        power_hold: POWER,
        delay: DELAY,
    ) -> Self {
        Self {
            spi,
            busy,
            dc,
            reset,
            power_hold,
            delay,
        }
    }
}

/// Initialization or display-control failure.
#[derive(Debug)]
pub enum Epaper1in54Error<SPI, DC, BUSY, RST, POWER> {
    /// Panel SPI transaction failed.
    Spi(SPI),
    /// The data/command output failed.
    DataCommand(DC),
    /// The `BUSY` input could not be read.
    Busy(BUSY),
    /// The reset output failed.
    Reset(RST),
    /// The Board power-hold output rejected the requested state.
    Power(POWER),
    /// `BUSY` never settled after reset, so no controller was identified.
    PanelNotDetected,
    /// The controller stayed busy past its timeout.
    Timeout,
    /// This panel has no brightness control.
    BrightnessUnsupported,
    /// This fixed panel does not rotate in hardware.
    OrientationUnsupported,
}

impl<SPI, DC, BUSY, RST, POWER> From<jd79653a::Error<SPI, DC, BUSY>>
    for Epaper1in54Error<SPI, DC, BUSY, RST, POWER>
{
    fn from(error: jd79653a::Error<SPI, DC, BUSY>) -> Self {
        match error {
            jd79653a::Error::Spi(error) => Self::Spi(error),
            jd79653a::Error::DataCommand(error) => Self::DataCommand(error),
            jd79653a::Error::Busy(error) => Self::Busy(error),
            jd79653a::Error::Timeout | jd79653a::Error::ShortFrame => Self::Timeout,
        }
    }
}

/// Error type of one concrete display.
pub type ErrorOf<SPI, BUSY, DC, RST, POWER> = Epaper1in54Error<
    <SPI as embedded_hal::spi::ErrorType>::Error,
    <DC as embedded_hal::digital::ErrorType>::Error,
    <BUSY as embedded_hal::digital::ErrorType>::Error,
    <RST as embedded_hal::digital::ErrorType>::Error,
    <POWER as embedded_hal::digital::ErrorType>::Error,
>;

enum Panel<SPI, BUSY, DC, RST, DELAY>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DC: OutputPin,
    RST: OutputPin,
    DELAY: DelayNs,
{
    Ssd1681 {
        epd: Epd1in54<SPI, BUSY, DC, RST, DELAY>,
        spi: SPI,
    },
    Jd79653a {
        controller: Jd79653a<SPI, DC, BUSY>,
        reset: RST,
        asleep: bool,
    },
}

/// Initialized buffered 1.54-inch e-paper display.
pub struct Epaper1in54Display<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DC: OutputPin,
    RST: OutputPin,
    POWER: OutputPin,
    DELAY: DelayNs,
{
    panel: Panel<SPI, BUSY, DC, RST, DELAY>,
    power_hold: POWER,
    delay: DELAY,
    framebuffer: Display1in54,
    /// The image last presented, which the next JD79653A update loads as the
    /// controller's "old" frame. The panel's content at start-up is unknown
    /// and treated as white.
    displayed: [u8; FRAME_BYTES],
}

/// Static implementation factory.
pub struct Epaper1in54DisplayImplementation<SPI, BUSY, DC, RST, POWER, DELAY>(
    PhantomData<(SPI, BUSY, DC, RST, POWER, DELAY)>,
);

fn reset_pulse<RST: OutputPin>(
    reset: &mut RST,
    delay: &mut impl DelayNs,
) -> Result<(), RST::Error> {
    reset.set_high()?;
    delay.delay_ms(1);
    reset.set_low()?;
    delay.delay_ms(RESET_LOW_MS);
    reset.set_high()?;
    delay.delay_ms(RESET_RECOVERY_MS);
    Ok(())
}

/// Samples `BUSY` until [`DETECT_SAMPLES`] consecutive reads agree.
fn detect<BUSY: InputPin>(
    busy: &mut BUSY,
    delay: &mut impl DelayNs,
) -> Result<Option<PanelController>, BUSY::Error> {
    delay.delay_ms(DETECT_SETTLE_MS);
    let mut previous = busy.is_high()?;
    let mut agreeing = 1_u32;
    for _ in 0..DETECT_MAX_SAMPLES {
        if agreeing >= DETECT_SAMPLES {
            return Ok(Some(PanelController::from_idle_busy_level(previous)));
        }
        delay.delay_ms(DETECT_SAMPLE_INTERVAL_MS);
        let level = busy.is_high()?;
        if level == previous {
            agreeing = agreeing.saturating_add(1);
        } else {
            previous = level;
            agreeing = 1;
        }
    }
    Ok(None)
}

impl<SPI, BUSY, DC, RST, POWER, DELAY> PeripheralImplementation
    for Epaper1in54DisplayImplementation<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice + 'static,
    BUSY: InputPin + 'static,
    DC: OutputPin + 'static,
    RST: OutputPin + 'static,
    POWER: OutputPin + 'static,
    DELAY: DelayNs + 'static,
    SPI::Error: 'static,
    BUSY::Error: 'static,
    DC::Error: 'static,
    RST::Error: 'static,
    POWER::Error: 'static,
{
    type Bindings = Epaper1in54Bindings<SPI, BUSY, DC, RST, POWER, DELAY>;
    type Config = ();
    type Peripheral = Epaper1in54Display<SPI, BUSY, DC, RST, POWER, DELAY>;
    type Error = ErrorOf<SPI, BUSY, DC, RST, POWER>;

    async fn initialize(
        bindings: Self::Bindings,
        (): Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let Epaper1in54Bindings {
            mut spi,
            mut busy,
            dc,
            mut reset,
            mut power_hold,
            mut delay,
        } = bindings;
        power_hold.set_high().map_err(Epaper1in54Error::Power)?;
        // M5GFX resets before identifying: a panel in deep sleep would not
        // answer otherwise.
        reset_pulse(&mut reset, &mut delay).map_err(Epaper1in54Error::Reset)?;
        let controller = detect(&mut busy, &mut delay)
            .map_err(Epaper1in54Error::Busy)?
            .ok_or(Epaper1in54Error::PanelNotDetected)?;
        let panel = match controller {
            PanelController::Ssd1681 => {
                let epd = Epd1in54::new(&mut spi, busy, dc, reset, &mut delay, None)
                    .map_err(Epaper1in54Error::Spi)?;
                Panel::Ssd1681 { epd, spi }
            }
            PanelController::Jd79653a => {
                let mut controller = Jd79653a::new(spi, dc, busy);
                controller.initialize(&mut delay)?;
                Panel::Jd79653a {
                    controller,
                    reset,
                    asleep: false,
                }
            }
        };
        Ok(Epaper1in54Display {
            panel,
            power_hold,
            delay,
            framebuffer: Display1in54::default(),
            displayed: [0xFF; FRAME_BYTES],
        })
    }
}

impl<SPI, BUSY, DC, RST, POWER, DELAY> Epaper1in54Display<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DC: OutputPin,
    RST: OutputPin,
    POWER: OutputPin,
    DELAY: DelayNs,
{
    /// Returns the controller identified at start-up.
    #[must_use]
    pub const fn controller(&self) -> PanelController {
        match self.panel {
            Panel::Ssd1681 { .. } => PanelController::Ssd1681,
            Panel::Jd79653a { .. } => PanelController::Jd79653a,
        }
    }
}

/// Byte-aligned JD79653A window covering `region`, or the whole panel.
fn jd_window(region: Option<Rectangle>) -> Window {
    let Some(region) = region else {
        return Window::FULL;
    };
    let panel = Rectangle::new(Point::zero(), Size::new(WIDTH, HEIGHT));
    let clipped = region.intersection(&panel);
    let Some(bottom_right) = clipped.bottom_right() else {
        return Window::FULL;
    };
    let coordinate = |value: i32| u16::try_from(value).unwrap_or(0);
    Window::covering(
        coordinate(clipped.top_left.x),
        coordinate(clipped.top_left.y),
        coordinate(bottom_right.x),
        coordinate(bottom_right.y),
    )
    .unwrap_or(Window::FULL)
}

impl<SPI, BUSY, DC, RST, POWER, DELAY> OriginDimensions
    for Epaper1in54Display<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DC: OutputPin,
    RST: OutputPin,
    POWER: OutputPin,
    DELAY: DelayNs,
{
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl<SPI, BUSY, DC, RST, POWER, DELAY> DrawTarget
    for Epaper1in54Display<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DC: OutputPin,
    RST: OutputPin,
    POWER: OutputPin,
    DELAY: DelayNs,
{
    type Color = Color;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        self.framebuffer.draw_iter(pixels)
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        self.framebuffer.fill_contiguous(area, colors)
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        self.framebuffer.fill_solid(area, color)
    }
}

impl<SPI, BUSY, DC, RST, POWER, DELAY> Display
    for Epaper1in54Display<SPI, BUSY, DC, RST, POWER, DELAY>
where
    SPI: SpiDevice,
    BUSY: InputPin,
    DC: OutputPin,
    RST: OutputPin,
    POWER: OutputPin,
    DELAY: DelayNs,
{
    type ControlError = ErrorOf<SPI, BUSY, DC, RST, POWER>;
    type RenderError = Infallible;

    fn descriptor(&self) -> DisplayDescriptor {
        DESCRIPTOR
    }

    fn draw_rgb565(&mut self, area: Rectangle, pixels: &[Rgb565]) -> Result<(), Self::RenderError> {
        // Each channel on the 0..=255 scale; white from half the summed range.
        let level = |value: u8, max: u16| u16::from(value) * 255 / max;
        self.fill_contiguous(
            &area,
            pixels.iter().map(|pixel| {
                if level(pixel.r(), 31) + level(pixel.g(), 63) + level(pixel.b(), 31) >= 384 {
                    Color::White
                } else {
                    Color::Black
                }
            }),
        )
    }

    async fn flush(&mut self, request: RefreshRequest) -> Result<(), Self::ControlError> {
        let quality = matches!(request.mode(), RefreshMode::Automatic | RefreshMode::Full);
        match &mut self.panel {
            Panel::Ssd1681 { epd, spi } => {
                let lut = if quality {
                    RefreshLut::Full
                } else {
                    RefreshLut::Quick
                };
                epd.set_lut(spi, &mut self.delay, Some(lut))
                    .map_err(Epaper1in54Error::Spi)?;
                epd.update_and_display_frame(spi, self.framebuffer.buffer(), &mut self.delay)
                    .map_err(Epaper1in54Error::Spi)
            }
            Panel::Jd79653a { controller, .. } => {
                // M5GFX `Panel_GDEW0154M09::display`: the old RAM holds the
                // presented image; a quality update first shows the inverted
                // image, then the image itself, to clear ghosting.
                let window = jd_window(request.region());
                let image = self.framebuffer.buffer();
                let delay = &mut self.delay;
                let displayed = &mut self.displayed;
                controller.write_frame(delay, FrameRam::Old, window, displayed, false)?;
                if quality {
                    controller.write_frame(delay, FrameRam::New, window, image, true)?;
                    controller.refresh(delay)?;
                    delay.delay_ms(JD79653A_REFRESH_MS);
                    controller.write_frame(delay, FrameRam::Old, window, image, true)?;
                }
                controller.write_frame(delay, FrameRam::New, window, image, false)?;
                controller.refresh(delay)?;
                delay.delay_ms(JD79653A_REFRESH_MS);
                displayed.copy_from_slice(image.get(..FRAME_BYTES).unwrap_or(&[0xFF; FRAME_BYTES]));
                Ok(())
            }
        }
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        if power == DisplayPower::On {
            self.power_hold
                .set_high()
                .map_err(Epaper1in54Error::Power)?;
        }
        match &mut self.panel {
            Panel::Ssd1681 { epd, spi } => match power {
                DisplayPower::On => epd
                    .wake_up(spi, &mut self.delay)
                    .map_err(Epaper1in54Error::Spi)?,
                DisplayPower::Sleep | DisplayPower::Off => epd
                    .sleep(spi, &mut self.delay)
                    .map_err(Epaper1in54Error::Spi)?,
            },
            Panel::Jd79653a {
                controller,
                reset,
                asleep,
                ..
            } => match power {
                DisplayPower::On => {
                    if *asleep {
                        // Deep sleep only ends with a hardware reset.
                        reset_pulse(reset, &mut self.delay).map_err(Epaper1in54Error::Reset)?;
                        controller.initialize(&mut self.delay)?;
                        *asleep = false;
                    } else {
                        controller.power_on(&mut self.delay)?;
                    }
                }
                DisplayPower::Sleep | DisplayPower::Off => {
                    controller.deep_sleep(&mut self.delay)?;
                    *asleep = true;
                }
            },
        }
        if power == DisplayPower::Off {
            self.power_hold.set_low().map_err(Epaper1in54Error::Power)?;
        }
        Ok(())
    }

    async fn set_brightness(&mut self, _brightness: u8) -> Result<(), Self::ControlError> {
        Err(Epaper1in54Error::BrightnessUnsupported)
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        if orientation == DisplayOrientation::Deg0 {
            Ok(())
        } else {
            Err(Epaper1in54Error::OrientationUnsupported)
        }
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        match &mut self.panel {
            Panel::Ssd1681 { epd, spi } => epd
                .wait_until_idle(spi, &mut self.delay)
                .map_err(Epaper1in54Error::Spi),
            Panel::Jd79653a { controller, .. } => {
                Ok(controller
                    .wait_until_idle(&mut self.delay, jd79653a::DEFAULT_BUSY_TIMEOUT_MS)?)
            }
        }
    }
}

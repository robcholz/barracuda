//! HDMI output of M5Stack's video bridge, as on the M5Stack ATOM Display.
//!
//! A Gowin GW1NR-9 FPGA holds the frame and takes drawing commands over SPI
//! (mode 3); a Lontium LT8618SX turns its RGB video into HDMI. Start-up
//! follows M5GFX `Panel_M5HDMI::init` (M5GFX 0.2.34):
//!
//! 1. identify the LT8618SX and stop its output;
//! 2. read the FPGA's ID; when it does not answer "HD", load M5Stack's
//!    design into its SRAM over JTAG on the same lines (TCK on SCK, TDI on
//!    MOSI, TMS on the chip select) and wait until it reads idle;
//! 3. read the ID again, up to eight times, lowering the write clock when
//!    the FPGA does not answer at all and the read clock when it answers
//!    wrongly;
//! 4. program the video timing, scaling, and pixel clock of the mode
//!    `barracuda_driver_m5hdmi::VideoMode` derives;
//! 5. start the LT8618SX, which fails unless its PLL locks, and clear the
//!    frame to black.
//!
//! The FPGA draws as commands arrive, so drawing presents at once. It
//! queues about 512 command bytes: as M5GFX does, the implementation reads
//! the FPGA's status (00h while busy) before a window and whenever 512
//! bytes have been sent, and ends a raw pixel stream by raising chip select.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::SpiBusControl;
use barracuda_driver_lt8618sx::{Error as TransmitterError, Lt8618sx};
use barracuda_driver_m5hdmi::{
    self as fpga, ID_PREFIX, JtagPort, LoadError, VideoMode, command, draw_pixel_rgb565,
    fill_rect_rgb565, video_timing, window,
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
    pixelcolor::{IntoStorage, Rgb565},
    primitives::{PointsIter, Rectangle},
};
use embedded_hal::{
    delay::DelayNs,
    digital::OutputPin,
    i2c::I2c,
    spi::{MODE_3, SpiBus},
};

/// The FPGA's pixel clock (M5GFX `M5ATOMDISPLAY_PIXELCLOCK`).
const PIXEL_CLOCK_HZ: u32 = 74_250_000;
/// Command bytes the FPGA queues before the host waits (M5GFX `_check_busy`).
const QUEUE_BYTES: usize = 512;
/// The clock M5GFX divides to lower the SPI clock (`getApbFrequency`).
const APB_HZ: u32 = 80_000_000;
/// ID reads before giving up (M5GFX: eight).
const ID_ATTEMPTS: usize = 8;
/// 10 ms polls for the FPGA to read idle after loading (M5GFX: 1024).
const IDLE_POLLS: usize = 1024;
/// Longest wait for a busy FPGA, in 1 µs steps growing by 1 µs (about 0.5 s).
const BUSY_POLLS: u32 = 1_000;
/// Bytes of one raw-pixel chunk.
const CHUNK_PIXELS: usize = 128;

/// Board-owned mode, transmitter address, and bus clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct M5HdmiDisplayConfig {
    width: u16,
    height: u16,
    refresh_hz: u32,
    transmitter_address: u8,
    write_hz: u32,
    read_hz: u32,
    jtag_hz: u32,
}

impl M5HdmiDisplayConfig {
    /// Creates configuration for a `width` × `height` drawing.
    #[must_use]
    pub const fn new(
        width: u16,
        height: u16,
        refresh_hz: u32,
        transmitter_address: u8,
        write_hz: u32,
        read_hz: u32,
        jtag_hz: u32,
    ) -> Self {
        Self {
            width,
            height,
            refresh_hz,
            transmitter_address,
            write_hz,
            read_hz,
            jtag_hz,
        }
    }
}

/// Move-only resources: the FPGA's bus and chip select, the transmitter's
/// bus, and a delay.
pub struct M5HdmiBindings<SPI, CS, I2C, DELAY> {
    spi: SPI,
    chip_select: CS,
    i2c: I2C,
    delay: DELAY,
}

impl<SPI, CS, I2C, DELAY> M5HdmiBindings<SPI, CS, I2C, DELAY> {
    /// Combines the resources.
    #[must_use]
    pub const fn new(spi: SPI, chip_select: CS, i2c: I2C, delay: DELAY) -> Self {
        Self {
            spi,
            chip_select,
            i2c,
            delay,
        }
    }
}

/// HDMI output failure.
#[derive(Debug)]
pub enum M5HdmiError<SpiError, PinError, I2cError> {
    /// A clock is zero or the transmitter address is not seven bits.
    InvalidConfig,
    /// An SPI transfer to the FPGA failed.
    Spi(SpiError),
    /// The chip select failed.
    Pin(PinError),
    /// The LT8618SX is missing, failed, or its PLL did not lock.
    Transmitter(TransmitterError<I2cError>),
    /// The bundled FPGA bitstream is malformed.
    MalformedBitstream,
    /// The FPGA did not read idle after its configuration was loaded.
    FpgaNotIdle,
    /// The FPGA's ID did not read "HD"; the last four bytes read.
    FpgaId([u8; 4]),
    /// The FPGA stayed busy.
    FpgaBusy,
    /// HDMI output has no brightness control.
    BrightnessUnsupported,
    /// The FPGA scans out in one orientation.
    OrientationUnsupported,
}

/// The error of a display over these bus types.
pub type ErrorFor<SPI, CS, I2C> = M5HdmiError<
    <SPI as embedded_hal::spi::ErrorType>::Error,
    <CS as embedded_hal::digital::ErrorType>::Error,
    <I2C as embedded_hal::i2c::ErrorType>::Error,
>;

/// Initialized HDMI output.
pub struct M5HdmiDisplay<SPI, CS, I2C, DELAY> {
    spi: SPI,
    chip_select: CS,
    i2c: I2C,
    delay: DELAY,
    transmitter: Lt8618sx,
    mode: VideoMode,
    write_hz: u32,
    read_hz: u32,
    /// Command bytes sent since the FPGA last read ready.
    queued: usize,
    /// Whether the last operation streamed raw pixels.
    after_raw: bool,
}

/// Static factory used by generated Board composition.
pub struct M5HdmiDisplayImplementation<SPI, CS, I2C, DELAY>(Parts<SPI, CS, I2C, DELAY>);

/// The bindings' types, held without owning a value of them.
type Parts<SPI, CS, I2C, DELAY> = PhantomData<fn() -> (SPI, CS, I2C, DELAY)>;

impl<SPI, CS, I2C, DELAY> PeripheralImplementation
    for M5HdmiDisplayImplementation<SPI, CS, I2C, DELAY>
where
    SPI: SpiBus + SpiBusControl + 'static,
    CS: OutputPin + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = M5HdmiBindings<SPI, CS, I2C, DELAY>;
    type Config = M5HdmiDisplayConfig;
    type Peripheral = M5HdmiDisplay<SPI, CS, I2C, DELAY>;
    type Error = ErrorFor<SPI, CS, I2C>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        if config.write_hz == 0 || config.read_hz == 0 || config.jtag_hz == 0 {
            return Err(M5HdmiError::InvalidConfig);
        }
        let transmitter =
            Lt8618sx::new(config.transmitter_address).ok_or(M5HdmiError::InvalidConfig)?;
        let M5HdmiBindings {
            spi,
            chip_select,
            i2c,
            delay,
        } = bindings;
        let mut display = M5HdmiDisplay {
            spi,
            chip_select,
            i2c,
            delay,
            transmitter,
            mode: VideoMode::new(
                config.width,
                config.height,
                config.refresh_hz,
                PIXEL_CLOCK_HZ,
            ),
            write_hz: config.write_hz,
            read_hz: config.read_hz,
            queued: 0,
            after_raw: false,
        };
        display.start(config.jtag_hz)?;
        Ok(display)
    }
}

impl<SPI, CS, I2C, DELAY> M5HdmiDisplay<SPI, CS, I2C, DELAY>
where
    SPI: SpiBus + SpiBusControl,
    CS: OutputPin,
    I2C: I2c,
    DELAY: DelayNs,
{
    /// Returns the output mode in use.
    #[must_use]
    pub const fn mode(&self) -> &VideoMode {
        &self.mode
    }

    fn start(&mut self, jtag_hz: u32) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        self.write_clock(self.write_hz)?;
        self.chip_select.set_high().map_err(M5HdmiError::Pin)?;
        let id = self
            .transmitter
            .chip_id(&mut self.i2c)
            .map_err(|error| M5HdmiError::Transmitter(TransmitterError::Bus(error)))?;
        if !id.is_present() {
            return Err(M5HdmiError::Transmitter(TransmitterError::NotPresent(id)));
        }
        self.transmitter.reset(&mut self.i2c);

        if self.read_fpga_id()?[..2] != ID_PREFIX {
            self.load_fpga(jtag_hz)?;
        }
        self.identify_fpga()?;

        // M5GFX `_init_resolution`: the timing, scaling, and pixel clock in
        // one transaction, outside the command count.
        let mode = self.mode;
        self.select()?;
        self.write(&video_timing(command::VIDEO_TIMING_V, mode.vertical))?;
        self.write(&video_timing(command::VIDEO_TIMING_H, mode.horizontal))?;
        self.write(&mode.scaling())?;
        self.write(&mode.clock.encode())?;
        self.deselect()?;

        self.transmitter
            .init(&mut self.i2c)
            .map_err(M5HdmiError::Transmitter)?;

        // M5GFX clears the screen once the panel is up.
        let (width, height) = mode.logical;
        self.fill(
            (0, 0, width.saturating_sub(1), height.saturating_sub(1)),
            Rgb565::new(0, 0, 0),
        )
    }

    /// Loads the FPGA's configuration and waits until it reads idle (M5GFX
    /// `LOAD_FPGA`, then "Waiting the FPGA gets idle").
    fn load_fpga(&mut self, jtag_hz: u32) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        self.spi
            .configure(jtag_hz, MODE_3)
            .map_err(M5HdmiError::Spi)?;
        let mut port = SpiJtag {
            spi: &mut self.spi,
            tms: &mut self.chip_select,
            delay: &mut self.delay,
            bytes_per_ms: jtag_hz.checked_div(8_000).unwrap_or(0).max(1),
        };
        let loaded = fpga::load_sram(&mut port, fpga::BITSTREAM_RLE);
        self.chip_select.set_high().map_err(M5HdmiError::Pin)?;
        self.write_clock(self.write_hz)?;
        match loaded {
            Ok(()) => {}
            Err(LoadError::Port(JtagError::Spi(error))) => return Err(M5HdmiError::Spi(error)),
            Err(LoadError::Port(JtagError::Pin(error))) => return Err(M5HdmiError::Pin(error)),
            Err(LoadError::MalformedBitstream) => return Err(M5HdmiError::MalformedBitstream),
        }

        self.select()?;
        self.read_clock()?;
        let mut word = [0u8; 4];
        let mut idle = false;
        self.read(&mut word)?;
        for _ in 0..IDLE_POLLS {
            self.delay.delay_ms(10);
            self.read(&mut word)?;
            if word == [0xff; 4] {
                idle = true;
                break;
            }
        }
        self.write_clock(self.write_hz)?;
        self.deselect()?;
        if idle {
            Ok(())
        } else {
            Err(M5HdmiError::FpgaNotIdle)
        }
    }

    /// Reads the ID until it answers "HD", lowering the write clock when the
    /// FPGA answers nothing and the read clock when it answers wrongly.
    fn identify_fpga(&mut self) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        let mut write_divider = divider(self.write_hz);
        let mut read_divider = divider(self.read_hz);
        let mut id = [0u8; 4];
        for _ in 0..ID_ATTEMPTS {
            id = self.read_fpga_id()?;
            if id[..2] == ID_PREFIX {
                return Ok(());
            }
            if id == [0; 4] || id == [0xff; 4] {
                write_divider = write_divider.saturating_add(1);
                self.write_hz = APB_HZ.checked_div(write_divider).unwrap_or(1);
                self.write_clock(self.write_hz)?;
            } else {
                read_divider = read_divider.saturating_add(1);
                self.read_hz = APB_HZ.checked_div(read_divider).unwrap_or(1);
            }
        }
        Err(M5HdmiError::FpgaId(id))
    }

    /// M5GFX `_read_fpga_id`: NOPs, a deselected gap, READ_ID, the 0xFF
    /// fill, one skipped byte, and the four ID bytes.
    fn read_fpga_id(&mut self) -> Result<[u8; 4], ErrorFor<SPI, CS, I2C>> {
        let nop = [command::NOP; 4];
        self.select()?;
        self.write(&nop)?;
        self.chip_select.set_high().map_err(M5HdmiError::Pin)?;
        self.write(&nop)?;
        self.chip_select.set_low().map_err(M5HdmiError::Pin)?;
        self.write(&[command::READ_ID])?;
        self.read_clock()?;
        let mut byte = [0u8; 1];
        for _ in 0..16 {
            self.read(&mut byte)?;
            if byte[0] != 0xff {
                break;
            }
        }
        self.read(&mut byte)?;
        let mut id = [0u8; 4];
        self.read(&mut id)?;
        self.write_clock(self.write_hz)?;
        self.deselect()?;
        Ok(id)
    }

    /// Sends one command (M5GFX `_check_busy` then the write): waits for the
    /// FPGA after a raw stream, before a window when anything is queued
    /// (`force`), and once 512 bytes are queued.
    fn command(&mut self, frame: &[u8], force: bool) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        if self.after_raw {
            self.after_raw = false;
            self.queued = 0;
            self.wait_fpga_ready()?;
        }
        let wait = if force && self.queued > 0 {
            true
        } else {
            self.queued = self.queued.saturating_add(frame.len());
            self.queued >= QUEUE_BYTES
        };
        if wait {
            self.queued = 0;
            self.wait_fpga_ready()?;
        }
        self.write(frame)
    }

    /// Reads status bytes until the FPGA answers non-zero, then pulses chip
    /// select, as M5GFX does after its wait.
    fn wait_fpga_ready(&mut self) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        self.read_clock()?;
        let mut status = [0u8; 1];
        let mut ready = false;
        for step in 0..BUSY_POLLS {
            self.read(&mut status)?;
            if status[0] != 0 {
                ready = true;
                break;
            }
            self.delay.delay_us(step.saturating_add(1));
        }
        self.write_clock(self.write_hz)?;
        self.chip_select.set_high().map_err(M5HdmiError::Pin)?;
        self.chip_select.set_low().map_err(M5HdmiError::Pin)?;
        if ready {
            Ok(())
        } else {
            Err(M5HdmiError::FpgaBusy)
        }
    }

    /// Streams `pixels` into the rectangle `x0..=x1` × `y0..=y1` of the
    /// logical frame, row by row.
    fn stream(
        &mut self,
        (x0, y0, x1, y1): (u16, u16, u16, u16),
        pixels: impl IntoIterator<Item = Rgb565>,
    ) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        let (ox, oy) = self.mode.offset;
        let frame = window(
            x0.saturating_add(ox),
            y0.saturating_add(oy),
            x1.saturating_add(ox),
            y1.saturating_add(oy),
            command::WRITE_RAW_16,
        );
        self.select()?;
        self.command(&frame, true)?;
        let mut chunk = [0u8; CHUNK_PIXELS * 2];
        let mut filled = 0usize;
        for color in pixels {
            if let Some(slot) = chunk.get_mut(filled..filled.saturating_add(2)) {
                slot.copy_from_slice(&color.into_storage().to_be_bytes());
            }
            filled = filled.saturating_add(2);
            if filled >= chunk.len() {
                self.write(&chunk)?;
                filled = 0;
            }
        }
        if filled > 0 {
            self.write(chunk.get(..filled).unwrap_or_default())?;
        }
        self.after_raw = true;
        self.deselect()
    }

    /// Fills `x0..=x1` × `y0..=y1` of the logical frame.
    fn fill(
        &mut self,
        (x0, y0, x1, y1): (u16, u16, u16, u16),
        color: Rgb565,
    ) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        let (ox, oy) = self.mode.offset;
        let frame = fill_rect_rgb565(
            x0.saturating_add(ox),
            y0.saturating_add(oy),
            x1.saturating_add(ox),
            y1.saturating_add(oy),
            color.into_storage(),
        );
        self.select()?;
        self.command(&frame, false)?;
        self.deselect()
    }

    /// The part of `area` on the logical frame, as inclusive corners.
    fn clip(&self, area: &Rectangle) -> Option<(u16, u16, u16, u16)> {
        let visible = area.intersection(&self.bounding_box());
        let bottom_right = visible.bottom_right()?;
        Some((
            u16::try_from(visible.top_left.x).ok()?,
            u16::try_from(visible.top_left.y).ok()?,
            u16::try_from(bottom_right.x).ok()?,
            u16::try_from(bottom_right.y).ok()?,
        ))
    }

    fn select(&mut self) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        self.chip_select.set_low().map_err(M5HdmiError::Pin)
    }

    fn deselect(&mut self) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        self.spi.flush().map_err(M5HdmiError::Spi)?;
        self.chip_select.set_high().map_err(M5HdmiError::Pin)
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        self.spi.write(bytes).map_err(M5HdmiError::Spi)?;
        self.spi.flush().map_err(M5HdmiError::Spi)
    }

    /// Reads while clocking out NOPs (zeros).
    fn read(&mut self, buffer: &mut [u8]) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        buffer.fill(command::NOP);
        self.spi
            .transfer_in_place(buffer)
            .map_err(M5HdmiError::Spi)?;
        self.spi.flush().map_err(M5HdmiError::Spi)
    }

    fn read_clock(&mut self) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        self.spi
            .configure(self.read_hz, MODE_3)
            .map_err(M5HdmiError::Spi)
    }

    fn write_clock(&mut self, frequency_hz: u32) -> Result<(), ErrorFor<SPI, CS, I2C>> {
        self.spi
            .configure(frequency_hz, MODE_3)
            .map_err(M5HdmiError::Spi)
    }
}

/// M5GFX's clock divider for `frequency_hz`: `APB / (frequency + 1) + 1`.
fn divider(frequency_hz: u32) -> u32 {
    APB_HZ
        .checked_div(frequency_hz.saturating_add(1))
        .unwrap_or(0)
        .saturating_add(1)
}

/// The JTAG port on the FPGA's SPI lines: TCK and TDI through the
/// controller, TMS on the chip select.
struct SpiJtag<'a, SPI, CS, DELAY> {
    spi: &'a mut SPI,
    tms: &'a mut CS,
    delay: &'a mut DELAY,
    bytes_per_ms: u32,
}

/// JTAG transport failure.
enum JtagError<SpiError, PinError> {
    Spi(SpiError),
    Pin(PinError),
}

impl<SPI, CS, DELAY> JtagPort for SpiJtag<'_, SPI, CS, DELAY>
where
    SPI: SpiBus + SpiBusControl,
    CS: OutputPin,
    DELAY: DelayNs,
{
    type Error = JtagError<SPI::Error, CS::Error>;

    fn set_tms(&mut self, high: bool) -> Result<(), Self::Error> {
        self.tms.set_state(high.into()).map_err(JtagError::Pin)
    }

    fn clock(&mut self, cycles: u8) -> Result<(), Self::Error> {
        let mut remaining = cycles;
        while remaining > 0 {
            let bits = remaining.min(8);
            self.spi.write_bits(0, bits).map_err(JtagError::Spi)?;
            remaining = remaining.saturating_sub(bits);
        }
        Ok(())
    }

    fn shift_bits(&mut self, value: u8, bits: u8) -> Result<(), Self::Error> {
        self.spi.write_bits(value, bits).map_err(JtagError::Spi)
    }

    fn shift_bytes(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.spi.write(bytes).map_err(JtagError::Spi)?;
        self.spi.flush().map_err(JtagError::Spi)
    }

    fn clock_for_ms(&mut self, ms: u32) -> Result<(), Self::Error> {
        let zeros = [0u8; 64];
        let mut remaining = usize::try_from(self.bytes_per_ms.saturating_mul(ms)).unwrap_or(0);
        while remaining > 0 {
            let chunk = remaining.min(zeros.len());
            self.shift_bytes(zeros.get(..chunk).unwrap_or_default())?;
            remaining = remaining.saturating_sub(chunk);
        }
        Ok(())
    }

    fn delay_ms(&mut self, ms: u32) -> Result<(), Self::Error> {
        self.delay.delay_ms(ms);
        Ok(())
    }
}

impl<SPI, CS, I2C, DELAY> OriginDimensions for M5HdmiDisplay<SPI, CS, I2C, DELAY> {
    fn size(&self) -> Size {
        let (width, height) = self.mode.logical;
        Size::new(u32::from(width), u32::from(height))
    }
}

impl<SPI, CS, I2C, DELAY> DrawTarget for M5HdmiDisplay<SPI, CS, I2C, DELAY>
where
    SPI: SpiBus + SpiBusControl,
    CS: OutputPin,
    I2C: I2c,
    DELAY: DelayNs,
{
    type Color = Rgb565;
    type Error = ErrorFor<SPI, CS, I2C>;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let size = self.size();
        let (ox, oy) = self.mode.offset;
        self.select()?;
        for Pixel(point, color) in pixels {
            let (Ok(x), Ok(y)) = (u16::try_from(point.x), u16::try_from(point.y)) else {
                continue;
            };
            if u32::from(x) >= size.width || u32::from(y) >= size.height {
                continue;
            }
            let frame = draw_pixel_rgb565(
                x.saturating_add(ox),
                y.saturating_add(oy),
                color.into_storage(),
            );
            self.command(&frame, false)?;
        }
        self.deselect()
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        let Some(corners) = self.clip(area) else {
            return Ok(());
        };
        let inside = area.intersection(&self.bounding_box()) == *area;
        if inside {
            let count = usize::try_from(area.size.width.saturating_mul(area.size.height))
                .unwrap_or(usize::MAX);
            self.stream(corners, colors.into_iter().take(count))
        } else {
            // Partly off the frame: draw the visible pixels one by one.
            self.draw_iter(
                area.points()
                    .zip(colors)
                    .map(|(point, color)| Pixel(point, color)),
            )
        }
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        match self.clip(area) {
            Some(corners) => self.fill(corners, color),
            None => Ok(()),
        }
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        let area = self.bounding_box();
        self.fill_solid(&area, color)
    }
}

impl<SPI, CS, I2C, DELAY> Display for M5HdmiDisplay<SPI, CS, I2C, DELAY>
where
    SPI: SpiBus + SpiBusControl,
    CS: OutputPin,
    I2C: I2c,
    DELAY: DelayNs,
{
    type ControlError = ErrorFor<SPI, CS, I2C>;
    type RenderError = ErrorFor<SPI, CS, I2C>;

    fn descriptor(&self) -> DisplayDescriptor {
        DisplayDescriptor::new(
            self.size(),
            PixelFormat::Rgb565,
            DisplayTechnology::VideoOutput,
            DisplayFeatures::SLEEP | DisplayFeatures::BUSY,
        )
    }

    fn draw_rgb565(&mut self, area: Rectangle, pixels: &[Rgb565]) -> Result<(), Self::RenderError> {
        self.fill_contiguous(&area, pixels.iter().copied())
    }

    async fn flush(&mut self, _request: RefreshRequest) -> Result<(), Self::ControlError> {
        // The FPGA draws each command as it arrives.
        Ok(())
    }

    async fn set_power(&mut self, power: DisplayPower) -> Result<(), Self::ControlError> {
        // M5GFX `setSleep`: the transmitter stops or restarts the output;
        // the FPGA keeps its frame.
        match power {
            DisplayPower::On => self
                .transmitter
                .init(&mut self.i2c)
                .map_err(M5HdmiError::Transmitter),
            DisplayPower::Sleep | DisplayPower::Off => {
                self.transmitter.reset(&mut self.i2c);
                Ok(())
            }
        }
    }

    async fn set_brightness(&mut self, _brightness: u8) -> Result<(), Self::ControlError> {
        Err(M5HdmiError::BrightnessUnsupported)
    }

    async fn set_orientation(
        &mut self,
        orientation: DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        if orientation == DisplayOrientation::Deg0 {
            Ok(())
        } else {
            Err(M5HdmiError::OrientationUnsupported)
        }
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        if self.queued == 0 && !self.after_raw {
            return Ok(());
        }
        self.select()?;
        self.after_raw = false;
        self.queued = 0;
        self.wait_fpga_ready()?;
        self.deselect()
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]
mod tests {
    extern crate std;

    use core::{cell::RefCell, convert::Infallible};
    use std::{collections::VecDeque, rc::Rc, vec::Vec};

    use barracuda_board_hal::SpiBusControl;
    use barracuda_peripheral::{PeripheralImplementation, display::Display};
    use embassy_futures::block_on;
    use embedded_graphics_core::{
        draw_target::DrawTarget,
        geometry::{OriginDimensions, Point, Size},
        pixelcolor::Rgb565,
        prelude::RgbColor,
        primitives::Rectangle,
    };
    use embedded_hal::{
        delay::DelayNs,
        digital::{ErrorType as PinErrorType, OutputPin},
        i2c::{ErrorType as I2cErrorType, I2c, Operation},
        spi::{ErrorType as SpiErrorType, Mode, SpiBus},
    };

    use super::{M5HdmiBindings, M5HdmiDisplayConfig, M5HdmiDisplayImplementation};

    /// What the FPGA's lines carried.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Event {
        Write(Vec<u8>),
        Bits(u8, u8),
    }

    /// An FPGA model: unconfigured until JTAG clocks reach it, then
    /// answering "HD" to READ_ID and ready (FFh) to every other read.
    #[derive(Default)]
    struct Fpga {
        selected: bool,
        configured: bool,
        answer: VecDeque<u8>,
        events: Vec<Event>,
        clocks: Vec<u32>,
    }

    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Fpga>>);

    struct Bus(Shared);
    struct ChipSelect(Shared);

    impl SpiErrorType for Bus {
        type Error = Infallible;
    }

    impl SpiBus for Bus {
        fn read(&mut self, words: &mut [u8]) -> Result<(), Infallible> {
            self.transfer_in_place(words)
        }

        fn write(&mut self, words: &[u8]) -> Result<(), Infallible> {
            let mut fpga = self.0.0.borrow_mut();
            if fpga.selected && fpga.configured && words == [0x04] {
                fpga.answer = [0xff, 0xff, 0x00, 0x00, b'H', b'D', 1, 2].into();
            }
            fpga.events.push(Event::Write(words.to_vec()));
            Ok(())
        }

        fn transfer(&mut self, read: &mut [u8], _write: &[u8]) -> Result<(), Infallible> {
            self.transfer_in_place(read)
        }

        fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Infallible> {
            let mut fpga = self.0.0.borrow_mut();
            for word in words {
                *word = fpga.answer.pop_front().unwrap_or(0xff);
            }
            Ok(())
        }

        fn flush(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
    }

    impl SpiBusControl for Bus {
        fn configure(&mut self, frequency_hz: u32, _mode: Mode) -> Result<(), Infallible> {
            self.0.0.borrow_mut().clocks.push(frequency_hz);
            Ok(())
        }

        fn write_bits(&mut self, value: u8, bits: u8) -> Result<(), Infallible> {
            let mut fpga = self.0.0.borrow_mut();
            fpga.configured = true;
            fpga.events.push(Event::Bits(value, bits));
            Ok(())
        }
    }

    impl PinErrorType for ChipSelect {
        type Error = Infallible;
    }

    impl OutputPin for ChipSelect {
        fn set_low(&mut self) -> Result<(), Infallible> {
            let mut fpga = self.0.0.borrow_mut();
            fpga.selected = true;
            fpga.answer.clear();
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Infallible> {
            self.0.0.borrow_mut().selected = false;
            Ok(())
        }
    }

    /// An LT8618SX whose PLL locks at once.
    struct Transmitter;

    impl I2cErrorType for Transmitter {
        type Error = Infallible;
    }

    impl I2c for Transmitter {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Infallible> {
            assert_eq!(address, 0x39);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => register = bytes[0],
                    Operation::Read(value) => {
                        value[0] = match register {
                            0x00 => 0x17,
                            0x01 => 0x02,
                            0x02 => 0xe1,
                            0x15 | 0xeb => 0x80,
                            0xea => 0x12,
                            _ => 0xff,
                        };
                    }
                }
            }
            Ok(())
        }
    }

    struct NoDelay;

    impl DelayNs for NoDelay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    fn start(
        configured: bool,
    ) -> (
        super::M5HdmiDisplay<Bus, ChipSelect, Transmitter, NoDelay>,
        Shared,
    ) {
        let shared = Shared::default();
        shared.0.borrow_mut().configured = configured;
        let display = block_on(M5HdmiDisplayImplementation::initialize(
            M5HdmiBindings::new(
                Bus(shared.clone()),
                ChipSelect(shared.clone()),
                Transmitter,
                NoDelay,
            ),
            M5HdmiDisplayConfig::new(1280, 720, 0, 0x39, 40_000_000, 16_000_000, 10_000_000),
        ))
        .expect("start");
        (display, shared)
    }

    fn writes(shared: &Shared) -> Vec<Vec<u8>> {
        shared
            .0
            .borrow()
            .events
            .iter()
            .filter_map(|event| match event {
                Event::Write(bytes) => Some(bytes.clone()),
                Event::Bits(..) => None,
            })
            .collect()
    }

    #[test]
    fn a_configured_fpga_is_not_loaded_and_gets_the_720p_mode() {
        let (display, shared) = start(true);
        assert!(
            !shared
                .0
                .borrow()
                .events
                .iter()
                .any(|event| matches!(event, Event::Bits(..)))
        );
        let sent = writes(&shared);
        let commands: Vec<u8> = sent
            .iter()
            .filter(|bytes| bytes.len() > 1)
            .map(|bytes| bytes[0])
            .filter(|opcode| *opcode != 0)
            .collect();
        // Vertical and horizontal timing, scaling, the pixel clock, then the
        // clear.
        assert_eq!(commands, [0xb0, 0xb1, 0x18, 0xb2, 0x6a]);
        assert_eq!(
            sent.last().expect("clear"),
            &[0x6a, 0, 0, 0, 0, 0x04, 0xff, 0x02, 0xcf, 0, 0]
        );
        assert_eq!(display.size(), Size::new(1280, 720));
    }

    #[test]
    fn an_unconfigured_fpga_is_loaded_over_jtag_first() {
        let (_display, shared) = start(false);
        let fpga = shared.0.borrow();
        assert!(
            fpga.events
                .iter()
                .any(|event| matches!(event, Event::Bits(..)))
        );
        assert!(fpga.clocks.contains(&10_000_000));
        // The bitstream's 444 430 bytes went out whole, apart from the
        // last, whose bits leave Shift-DR one by one, beside 4 ms of idle
        // clocks twice at 10 MHz.
        let streamed: usize = fpga
            .events
            .iter()
            .filter_map(|event| match event {
                Event::Write(bytes) => Some(bytes.len()),
                Event::Bits(..) => None,
            })
            .sum();
        assert!(streamed >= 444_429 + 2 * 5_000);
    }

    #[test]
    fn drawing_streams_raw_pixels_into_a_window_and_fills_rectangles() {
        let (mut display, shared) = start(true);
        shared.0.borrow_mut().events.clear();
        display
            .draw_rgb565(
                Rectangle::new(Point::new(2, 3), Size::new(2, 1)),
                &[Rgb565::RED, Rgb565::BLUE],
            )
            .expect("draw");
        display
            .fill_solid(
                &Rectangle::new(Point::new(1270, 710), Size::new(20, 20)),
                Rgb565::GREEN,
            )
            .expect("fill");
        let sent = writes(&shared);
        assert!(sent.contains(&std::vec![0x2a, 0, 2, 0, 3, 0x2b, 0, 3, 0, 3, 0x42]));
        assert!(sent.contains(&std::vec![0xf8, 0x00, 0x00, 0x1f]));
        // Clipped to the frame's last pixel.
        assert!(sent.contains(&std::vec![
            0x6a, 0x04, 0xf6, 0x02, 0xc6, 0x04, 0xff, 0x02, 0xcf, 0x07, 0xe0
        ]));
    }
}

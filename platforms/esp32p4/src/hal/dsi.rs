//! MIPI DSI host: the narrow command and frame channel a DSI display
//! implementation drives.
//!
//! The Platform owns the DSI PHY, the VDMA scanout channel, the framebuffer,
//! and the backlight pin. The display implementation owns the panel: it picks
//! the link and video timing, writes the controller's command sequence, and
//! decides when video starts.
//!
//! esp-hal 1.2 has no LEDC or MCPWM driver for the ESP32-P4, so the backlight
//! pin is switched on and off rather than dimmed.

use barracuda_bulk_memory::BulkBox;
use barracuda_peripheral::display::{DsiHost as DsiHostContract, DsiPanelConfig};
use esp_hal::{
    gpio::{AnyPin, Level, Output, OutputConfig, OutputPin},
    mipi_dsi::{
        dbi,
        dpi::{ColorFormat, DpiClockSource, DpiConfig, DsiDpi, FrameTiming},
        Config as LinkConfig, ConfigError as LinkConfigError, DataLanes, MipiDsi,
    },
    peripherals::{MIPI_DSI, VDMA_CH0, VDMA_CH1, VDMA_CH2, VDMA_CH3},
};

/// The on-chip LDO channel esp-hal powers the DSI PHY from (VO3).
const PHY_POWER_CHANNEL: u8 = 3;
/// The PHY supply esp-hal programs on that channel, in millivolts.
const PHY_POWER_MILLIVOLTS: u16 = 2_500;
/// DSI virtual channel used for both commands and video.
const VIRTUAL_CHANNEL: u8 = 0;
/// Scanout DMA reads the framebuffer in 64-byte bursts from PSRAM.
const FRAMEBUFFER_ALIGNMENT: usize = 64;
/// RGB565 bytes per pixel.
const BYTES_PER_PIXEL: usize = 2;

/// DCS commands the host sends for its own display and sleep controls.
const DCS_ENTER_SLEEP_MODE: u8 = 0x10;
const DCS_EXIT_SLEEP_MODE: u8 = 0x11;
const DCS_SET_DISPLAY_OFF: u8 = 0x28;
const DCS_SET_DISPLAY_ON: u8 = 0x29;

/// The VDMA channel that scans the framebuffer out, whichever of the four the
/// Board names.
pub struct ScanoutChannel(Scanout);

enum Scanout {
    Channel0(VDMA_CH0<'static>),
    Channel1(VDMA_CH1<'static>),
    Channel2(VDMA_CH2<'static>),
    Channel3(VDMA_CH3<'static>),
}

impl From<VDMA_CH0<'static>> for ScanoutChannel {
    fn from(channel: VDMA_CH0<'static>) -> Self {
        Self(Scanout::Channel0(channel))
    }
}

impl From<VDMA_CH1<'static>> for ScanoutChannel {
    fn from(channel: VDMA_CH1<'static>) -> Self {
        Self(Scanout::Channel1(channel))
    }
}

impl From<VDMA_CH2<'static>> for ScanoutChannel {
    fn from(channel: VDMA_CH2<'static>) -> Self {
        Self(Scanout::Channel2(channel))
    }
}

impl From<VDMA_CH3<'static>> for ScanoutChannel {
    fn from(channel: VDMA_CH3<'static>) -> Self {
        Self(Scanout::Channel3(channel))
    }
}

impl ScanoutChannel {
    fn open(
        self,
        dsi: MIPI_DSI<'static>,
        config: LinkConfig,
    ) -> Result<MipiDsi<'static>, LinkConfigError> {
        match self.0 {
            Scanout::Channel0(channel) => MipiDsi::new(dsi, channel, config),
            Scanout::Channel1(channel) => MipiDsi::new(dsi, channel, config),
            Scanout::Channel2(channel) => MipiDsi::new(dsi, channel, config),
            Scanout::Channel3(channel) => MipiDsi::new(dsi, channel, config),
        }
    }
}

/// The Board's backlight wiring, as generated composition hands it over.
pub struct DsiBacklight(Option<AnyPin<'static>>);

/// Another chip on the Board drives the backlight.
#[must_use]
pub fn dsi_backlight_none() -> DsiBacklight {
    DsiBacklight(None)
}

/// The chip drives the backlight from this pin.
#[must_use]
pub fn dsi_backlight_pin(pin: impl OutputPin + 'static) -> DsiBacklight {
    DsiBacklight(Some(pin.degrade()))
}

/// The backlight pin, switched on for any non-zero level; off until video
/// starts.
struct Backlight(Option<Output<'static>>);

impl Backlight {
    fn new(wiring: DsiBacklight) -> Self {
        Self(
            wiring
                .0
                .map(|pin| Output::new(pin, Level::Low, OutputConfig::default())),
        )
    }

    fn set(&mut self, brightness: u8) -> Result<(), DsiError> {
        let pin = self.0.as_mut().ok_or(DsiError::NoBacklight)?;
        pin.set_level(if brightness == 0 {
            Level::Low
        } else {
            Level::High
        });
        Ok(())
    }

    const fn present(&self) -> bool {
        self.0.is_some()
    }
}

/// The RGB565 framebuffer the link scans out, in bulk memory.
struct Frame {
    pixels: &'static mut [u8],
}

enum Link {
    /// The tokens, before a display implementation picked the link timing.
    Unconfigured {
        dsi: MIPI_DSI<'static>,
        scanout: ScanoutChannel,
    },
    /// The link is up in command mode; video has not started.
    Command {
        bus: MipiDsi<'static>,
        video: DpiConfig,
        frame: Frame,
    },
    /// The framebuffer streams to the panel.
    Video { stream: VideoStream },
    /// A failed transition left no usable link.
    Lost,
}

/// esp-hal's video stream, owned by one display.
struct VideoStream(DsiDpi<'static>);

// SAFETY: `DsiDpi` is not `Send` only because it keeps raw pointers to its
// framebuffers. They point into a leaked bulk-memory allocation that lives for
// the program and that nothing else references, and the stream is reached only
// through `&mut DsiHost`, so moving it to another task cannot share it.
#[allow(unsafe_code)]
unsafe impl Send for VideoStream {}

/// The ESP32-P4 MIPI DSI host one display implementation drives.
pub struct DsiHost {
    lanes: DataLanes,
    link: Link,
    width: usize,
    height: usize,
    backlight: Backlight,
    brightness_set: bool,
}

/// DSI host construction failure from Board wiring.
#[derive(Debug)]
pub enum DsiConfigError {
    /// The ESP32-P4 DSI PHY has one or two data lanes.
    UnsupportedLaneCount(u8),
    /// esp-hal powers the PHY only from LDO channel 3 at 2500 mV.
    UnsupportedPhyPower {
        /// Board-declared LDO channel.
        channel: u8,
        /// Board-declared supply in millivolts.
        millivolts: u16,
    },
}

impl core::fmt::Display for DsiConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsupportedLaneCount(lanes) => {
                write!(formatter, "the ESP32-P4 DSI PHY has 1 or 2 data lanes, not {lanes}")
            }
            Self::UnsupportedPhyPower {
                channel,
                millivolts,
            } => write!(
                formatter,
                "the DSI PHY is powered from LDO channel {PHY_POWER_CHANNEL} at {PHY_POWER_MILLIVOLTS} mV, not channel {channel} at {millivolts} mV"
            ),
        }
    }
}

impl core::error::Error for DsiConfigError {}

/// DSI host operation failure.
#[derive(Debug)]
pub enum DsiError {
    /// The operation needs [`DsiHostContract::initialize`] first.
    NotInitialized,
    /// The link was already initialized.
    AlreadyInitialized,
    /// The link or video configuration was rejected.
    Link(LinkConfigError),
    /// A DCS command was too large for the command FIFO.
    Command(dbi::Error),
    /// The panel's framebuffer does not fit in memory.
    FramebufferAllocation,
    /// A drawn rectangle leaves the panel or does not match its pixels.
    OutOfBounds,
    /// The Board drives the backlight through another chip.
    NoBacklight,
    /// A failed transition left the link unusable.
    LinkLost,
}

impl core::fmt::Display for DsiError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotInitialized => formatter.write_str("the DSI link is not initialized"),
            Self::AlreadyInitialized => formatter.write_str("the DSI link is already initialized"),
            Self::Link(error) => write!(formatter, "DSI link configuration: {error:?}"),
            Self::Command(error) => write!(formatter, "DSI command: {error:?}"),
            Self::FramebufferAllocation => formatter.write_str("no memory for the DSI framebuffer"),
            Self::OutOfBounds => formatter.write_str("pixels outside the DSI framebuffer"),
            Self::NoBacklight => formatter.write_str("this Board drives no backlight pin"),
            Self::LinkLost => formatter.write_str("the DSI link was lost"),
        }
    }
}

impl core::error::Error for DsiError {}

/// Takes the Board's DSI controller, scanout channel, and backlight wiring.
///
/// The link stays down until the display implementation initializes it with
/// its panel's timing.
///
/// # Errors
///
/// Returns [`DsiConfigError`] for a lane count or PHY supply the ESP32-P4
/// cannot provide.
pub fn dsi_host(
    dsi: MIPI_DSI<'static>,
    scanout: impl Into<ScanoutChannel>,
    backlight: DsiBacklight,
    data_lanes: u8,
    phy_power_channel: u8,
    phy_power_millivolts: u16,
) -> Result<DsiHost, DsiConfigError> {
    let lanes = match data_lanes {
        1 => DataLanes::_1,
        2 => DataLanes::_2,
        other => return Err(DsiConfigError::UnsupportedLaneCount(other)),
    };
    if phy_power_channel != PHY_POWER_CHANNEL || phy_power_millivolts != PHY_POWER_MILLIVOLTS {
        return Err(DsiConfigError::UnsupportedPhyPower {
            channel: phy_power_channel,
            millivolts: phy_power_millivolts,
        });
    }
    Ok(DsiHost {
        lanes,
        link: Link::Unconfigured {
            dsi,
            scanout: scanout.into(),
        },
        width: 0,
        height: 0,
        backlight: Backlight::new(backlight),
        brightness_set: false,
    })
}

/// Picks the DPI clock source whose integer divider lands closest to the
/// requested pixel clock, as ESP-IDF's driver divides one of the two PLLs.
fn dpi_clock_source(pixel_clock_mhz: u32) -> DpiClockSource {
    let error = |source_mhz: u32| {
        let divider = (source_mhz + pixel_clock_mhz / 2) / pixel_clock_mhz.max(1);
        (source_mhz / divider.max(1)).abs_diff(pixel_clock_mhz)
    };
    if error(160) < error(240) {
        DpiClockSource::PllF160m
    } else {
        DpiClockSource::PllF240m
    }
}

/// Allocates a zeroed RGB565 framebuffer, aligned for scanout, for the rest
/// of the program.
fn framebuffer(width: usize, height: usize) -> Result<Frame, DsiError> {
    let bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(BYTES_PER_PIXEL))
        .ok_or(DsiError::FramebufferAllocation)?;
    let storage = BulkBox::<[u8]>::try_new_zeroed_slice(bytes + FRAMEBUFFER_ALIGNMENT)
        .map_err(|_error| DsiError::FramebufferAllocation)?
        .leak();
    let offset = storage.as_ptr().align_offset(FRAMEBUFFER_ALIGNMENT);
    let pixels = storage
        .get_mut(offset..offset + bytes)
        .ok_or(DsiError::FramebufferAllocation)?;
    Ok(Frame { pixels })
}

impl DsiHost {
    fn pixels(&mut self) -> Result<&mut [u8], DsiError> {
        match &mut self.link {
            Link::Command { frame, .. } => Ok(&mut *frame.pixels),
            Link::Video { stream } => Ok(stream.0.framebuffer_mut()),
            Link::Unconfigured { .. } => Err(DsiError::NotInitialized),
            Link::Lost => Err(DsiError::LinkLost),
        }
    }
}

impl DsiHostContract for DsiHost {
    type Error = DsiError;

    fn initialize(&mut self, config: DsiPanelConfig) -> Result<(), Self::Error> {
        let (dsi, scanout) = match core::mem::replace(&mut self.link, Link::Lost) {
            Link::Unconfigured { dsi, scanout } => (dsi, scanout),
            other => {
                self.link = other;
                return Err(DsiError::AlreadyInitialized);
            }
        };
        let timing = config.timing;
        let width = usize::from(timing.width);
        let height = usize::from(timing.height);
        let frame = framebuffer(width, height)?;
        let mut bus = scanout
            .open(
                dsi,
                LinkConfig::default()
                    .with_num_data_lanes(self.lanes)
                    .with_lane_bit_rate_mbps(config.lane_bit_rate_mbps as f32),
            )
            .map_err(DsiError::Link)?;
        // Opening the command interface selects low-power transmission for
        // every command packet; video mode keeps that setting.
        bus.dbi(VIRTUAL_CHANNEL);
        let video = DpiConfig {
            virtual_channel: VIRTUAL_CHANNEL,
            pixel_clock_mhz: config.dpi_clock_mhz as f32,
            dpi_clk_src: dpi_clock_source(config.dpi_clock_mhz),
            in_color_format: ColorFormat::Rgb565,
            out_color_format: ColorFormat::Rgb565,
            timing: FrameTiming {
                h_active: u32::from(timing.width),
                hsw: u32::from(timing.hsync_pulse_width),
                hbp: u32::from(timing.hsync_back_porch),
                hfp: u32::from(timing.hsync_front_porch),
                v_active: u32::from(timing.height),
                vsw: u32::from(timing.vsync_pulse_width),
                vbp: u32::from(timing.vsync_back_porch),
                vfp: u32::from(timing.vsync_front_porch),
            },
        };
        self.width = width;
        self.height = height;
        self.link = Link::Command { bus, video, frame };
        Ok(())
    }

    fn start_video(&mut self) -> Result<(), Self::Error> {
        match core::mem::replace(&mut self.link, Link::Lost) {
            Link::Command { bus, video, frame } => {
                let stream = bus.dpi(video, &[frame.pixels]).map_err(DsiError::Link)?;
                self.link = Link::Video {
                    stream: VideoStream(stream),
                };
            }
            Link::Unconfigured { dsi, scanout } => {
                self.link = Link::Unconfigured { dsi, scanout };
                return Err(DsiError::NotInitialized);
            }
            video @ Link::Video { .. } => self.link = video,
            Link::Lost => return Err(DsiError::LinkLost),
        }
        if !self.brightness_set && self.backlight.present() {
            self.backlight.set(u8::MAX)?;
        }
        Ok(())
    }

    fn write_command(&mut self, command: u8, data: &[u8]) -> Result<(), Self::Error> {
        match &mut self.link {
            Link::Command { bus, .. } => bus
                .dbi(VIRTUAL_CHANNEL)
                .write_cmd(command, data)
                .map_err(DsiError::Command),
            Link::Video { .. } => video_command::write(VIRTUAL_CHANNEL, command, data),
            Link::Unconfigured { .. } => Err(DsiError::NotInitialized),
            Link::Lost => Err(DsiError::LinkLost),
        }
    }

    fn present(&mut self) -> Result<(), Self::Error> {
        match &mut self.link {
            // Starting video writes the whole framebuffer back first.
            Link::Command { .. } => Ok(()),
            Link::Video { stream } => {
                stream.0.commit();
                Ok(())
            }
            Link::Unconfigured { .. } => Err(DsiError::NotInitialized),
            Link::Lost => Err(DsiError::LinkLost),
        }
    }

    fn draw_rgb565(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        pixels: &[u16],
    ) -> Result<(), Self::Error> {
        let (x, y, width, height) = (
            usize::from(x),
            usize::from(y),
            usize::from(width),
            usize::from(height),
        );
        let stride = self.width;
        if x + width > self.width || y + height > self.height || pixels.len() != width * height {
            return Err(DsiError::OutOfBounds);
        }
        if width == 0 {
            return Ok(());
        }
        let framebuffer = self.pixels()?;
        for (row, source) in pixels.chunks_exact(width).enumerate() {
            let start = ((y + row) * stride + x) * BYTES_PER_PIXEL;
            let target = framebuffer
                .get_mut(start..start + width * BYTES_PER_PIXEL)
                .ok_or(DsiError::OutOfBounds)?;
            for (bytes, pixel) in target
                .as_chunks_mut::<BYTES_PER_PIXEL>()
                .0
                .iter_mut()
                .zip(source)
            {
                bytes.copy_from_slice(&pixel.to_le_bytes());
            }
        }
        Ok(())
    }

    fn set_display_enabled(&mut self, enabled: bool) -> Result<(), Self::Error> {
        let command = if enabled {
            DCS_SET_DISPLAY_ON
        } else {
            DCS_SET_DISPLAY_OFF
        };
        self.write_command(command, &[])
    }

    fn set_sleep(&mut self, sleep: bool) -> Result<(), Self::Error> {
        let command = if sleep {
            DCS_ENTER_SLEEP_MODE
        } else {
            DCS_EXIT_SLEEP_MODE
        };
        self.write_command(command, &[])
    }

    fn set_backlight(&mut self, brightness: u8) -> Result<(), Self::Error> {
        self.brightness_set = true;
        self.backlight.set(brightness)
    }
}

/// DCS writes while the link streams video.
///
/// esp-hal's command interface borrows the link only before video starts, so
/// commands sent later (display on and off, sleep, and panels whose
/// controllers take their initialization sequence during video) go straight
/// to the DSI host's generic packet registers. The host sends them in
/// low-power mode during blanking, as configured when the link opened. The
/// sequence follows esp-hal's `DsiDbi::write_cmd` and ESP-IDF's
/// `mipi_dsi_hal_host_gen_write_*_packet`.
mod video_command {
    use esp_hal::peripherals::MIPI_DSI_HOST;

    use super::DsiError;

    /// DCS short write with no parameter (MIPI DSI data type 0x05).
    const DCS_SHORT_WRITE_0: u8 = 0x05;
    /// DCS short write with one parameter (MIPI DSI data type 0x15).
    const DCS_SHORT_WRITE_1: u8 = 0x15;
    /// DCS long write (MIPI DSI data type 0x39).
    const DCS_LONG_WRITE: u8 = 0x39;

    pub(super) fn write(virtual_channel: u8, command: u8, data: &[u8]) -> Result<(), DsiError> {
        let payload = data.len() + 1;
        let word_count = u16::try_from(payload)
            .map_err(|_error| DsiError::Command(esp_hal::mipi_dsi::dbi::Error::PayloadTooLarge))?;
        let (data_type, low, high) = match data {
            [] => (DCS_SHORT_WRITE_0, command, 0),
            [parameter] => (DCS_SHORT_WRITE_1, command, *parameter),
            _ => {
                // The payload is the command byte followed by its parameters,
                // packed little-endian into 32-bit FIFO words.
                let mut word = [command, 0, 0, 0];
                let mut filled = 1;
                for byte in data {
                    word[filled] = *byte;
                    filled += 1;
                    if filled == word.len() {
                        push_payload(u32::from_le_bytes(word));
                        word = [0; 4];
                        filled = 0;
                    }
                }
                if filled != 0 {
                    push_payload(u32::from_le_bytes(word));
                }
                let [low, high] = word_count.to_le_bytes();
                (DCS_LONG_WRITE, low, high)
            }
        };
        push_header(virtual_channel, data_type, low, high);
        Ok(())
    }

    fn push_payload(word: u32) {
        let host = MIPI_DSI_HOST::regs();
        while host.cmd_pkt_status().read().gen_pld_w_full().bit_is_set() {}
        #[allow(unsafe_code)]
        // SAFETY: any 32-bit value is a valid generic payload FIFO word.
        host.gen_pld_data().write(|w| unsafe { w.bits(word) });
    }

    fn push_header(virtual_channel: u8, data_type: u8, low: u8, high: u8) {
        let host = MIPI_DSI_HOST::regs();
        while host.cmd_pkt_status().read().gen_cmd_full().bit_is_set() {}
        #[allow(unsafe_code)]
        // SAFETY: the virtual channel is 0-3, the data type is a DSI write
        // packet type, and the two word-count bytes take any value.
        host.gen_hdr().write(|w| unsafe {
            w.gen_vc().bits(virtual_channel);
            w.gen_dt().bits(data_type);
            w.gen_wc_lsbyte().bits(low);
            w.gen_wc_msbyte().bits(high)
        });
    }
}

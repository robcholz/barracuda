//! MIPI DBI type C display panel: an SPI link plus a data/command (D/C) pin,
//! driven with the MIPI Display Command Set (DCS v1.02.00).
//!
//! The ST7789, ILI9342C, ST7735S, GC9107, and GC9A01 controllers the
//! `mipi-dbi-display` implementation drives share the DCS commands modelled
//! here. A byte clocked while D/C is low is a command; bytes clocked while it
//! is high are that command's parameters, or pixels after `RAMWR`.
//!
//! The model keeps the picture as the driver addresses it: column and page
//! addresses map straight to pixels after the Board's panel offsets, and the
//! row/column exchange bit of `MADCTL` swaps the addressed width and height.
//! The mirror bits only compensate for how a panel is mounted, so they leave
//! the picture as the driver meant it. Colour order and inversion apply when
//! the picture is read, as the panel would show it.

use crate::{
    device::{DeviceContext, RegisterRangeError},
    spi::{Frame, SpiDevice},
};

/// DCS command codes (DCS v1.02.00, Table 4).
mod dcs {
    pub const SOFT_RESET: u8 = 0x01;
    pub const ENTER_SLEEP_MODE: u8 = 0x10;
    pub const EXIT_SLEEP_MODE: u8 = 0x11;
    pub const EXIT_INVERT_MODE: u8 = 0x20;
    pub const ENTER_INVERT_MODE: u8 = 0x21;
    pub const SET_DISPLAY_OFF: u8 = 0x28;
    pub const SET_DISPLAY_ON: u8 = 0x29;
    pub const SET_COLUMN_ADDRESS: u8 = 0x2a;
    pub const SET_PAGE_ADDRESS: u8 = 0x2b;
    pub const WRITE_MEMORY_START: u8 = 0x2c;
    pub const SET_ADDRESS_MODE: u8 = 0x36;
    pub const SET_PIXEL_FORMAT: u8 = 0x3a;
    pub const WRITE_MEMORY_CONTINUE: u8 = 0x3c;
}

/// `set_address_mode` bits (DCS v1.02.00, 6.2.4).
const ROW_COLUMN_EXCHANGE: u8 = 0x20;
const BGR: u8 = 0x08;

/// Rule identifiers for the traffic the model rejects.
mod rules {
    pub const NO_DC_PIN: &str = "dbi/dc-pin";
    pub const PIXEL_FORMAT: &str = "dcs/6.2.6-pixel-format";
    pub const PIXEL_OUTSIDE: &str = "dcs/6.2.10-write-window";
}

/// A display panel model on one SPI controller.
#[derive(Debug)]
pub struct MipiDbiPanelModel {
    /// Width and height as the Board declares the panel.
    size: (u16, u16),
    /// Column and page address of the panel's first visible pixel.
    offset: (u16, u16),
    /// Chip index of the D/C pin.
    dc: usize,
    pixels: Vec<u16>,
    address_mode: u8,
    sixteen_bit: bool,
    inverted: bool,
    sleeping: bool,
    on: bool,
    columns: (u16, u16),
    pages: (u16, u16),
    cursor: (u16, u16),
    command: Option<u8>,
    parameters: Vec<u8>,
    /// First byte of a pixel whose second byte has not arrived.
    half_pixel: Option<u8>,
    revision: u64,
}

impl MipiDbiPanelModel {
    /// A powered-on panel of `width` × `height` pixels whose D/C line is the
    /// pin with chip index `dc`, asleep and off as after reset (DCS 6.1).
    #[must_use]
    pub fn new(width: u16, height: u16, offset: (u16, u16), dc: usize) -> Self {
        Self {
            size: (width, height),
            offset,
            dc,
            pixels: vec![0; usize::from(width) * usize::from(height)],
            address_mode: 0,
            sixteen_bit: false,
            inverted: false,
            sleeping: true,
            on: false,
            columns: (0, width.saturating_sub(1)),
            pages: (0, height.saturating_sub(1)),
            cursor: (0, 0),
            command: None,
            parameters: Vec::new(),
            half_pixel: None,
            revision: 0,
        }
    }

    /// Width and height as the driver addresses them.
    const fn addressed_size(&self) -> (u16, u16) {
        if self.address_mode & ROW_COLUMN_EXCHANGE == 0 {
            self.size
        } else {
            (self.size.1, self.size.0)
        }
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn command(&mut self, context: &mut DeviceContext<'_>, command: u8) {
        self.command = Some(command);
        self.parameters.clear();
        self.half_pixel = None;
        match command {
            dcs::SOFT_RESET => {
                let (width, height) = self.size;
                let pixels = std::mem::take(&mut self.pixels);
                *self = Self {
                    pixels,
                    ..Self::new(width, height, self.offset, self.dc)
                };
                self.revision = self.revision.wrapping_add(1);
                context.note("soft reset");
            }
            dcs::ENTER_SLEEP_MODE => self.set(|panel| panel.sleeping = true),
            dcs::EXIT_SLEEP_MODE => self.set(|panel| panel.sleeping = false),
            dcs::EXIT_INVERT_MODE => self.set(|panel| panel.inverted = false),
            dcs::ENTER_INVERT_MODE => self.set(|panel| panel.inverted = true),
            dcs::SET_DISPLAY_OFF => self.set(|panel| panel.on = false),
            dcs::SET_DISPLAY_ON => self.set(|panel| panel.on = true),
            dcs::WRITE_MEMORY_START => self.cursor = (self.columns.0, self.pages.0),
            _ => {}
        }
    }

    fn set(&mut self, change: impl FnOnce(&mut Self)) {
        change(self);
        self.changed();
    }

    pub(crate) fn data(&mut self, context: &mut DeviceContext<'_>, bytes: &[u8]) {
        match self.command {
            Some(dcs::WRITE_MEMORY_START | dcs::WRITE_MEMORY_CONTINUE) => {
                self.pixels(context, bytes);
            }
            Some(command) => {
                self.parameters.extend_from_slice(bytes);
                self.parameters_received(context, command);
            }
            None => {}
        }
    }

    fn parameters_received(&mut self, context: &mut DeviceContext<'_>, command: u8) {
        let parameters = self.parameters.as_slice();
        match (command, parameters) {
            (dcs::SET_COLUMN_ADDRESS, [start_high, start_low, end_high, end_low, ..]) => {
                let offset = self.offset.0;
                self.columns = (
                    u16::from_be_bytes([*start_high, *start_low]).wrapping_sub(offset),
                    u16::from_be_bytes([*end_high, *end_low]).wrapping_sub(offset),
                );
            }
            (dcs::SET_PAGE_ADDRESS, [start_high, start_low, end_high, end_low, ..]) => {
                let offset = self.offset.1;
                self.pages = (
                    u16::from_be_bytes([*start_high, *start_low]).wrapping_sub(offset),
                    u16::from_be_bytes([*end_high, *end_low]).wrapping_sub(offset),
                );
            }
            (dcs::SET_ADDRESS_MODE, [mode, ..]) => {
                let exchanged = (self.address_mode ^ mode) & ROW_COLUMN_EXCHANGE != 0;
                self.address_mode = *mode;
                if exchanged {
                    self.pixels.fill(0);
                }
                self.changed();
            }
            (dcs::SET_PIXEL_FORMAT, [format, ..]) => {
                // DBI bits 2..0 = 101b select 16 bits per pixel.
                self.sixteen_bit = format & 0x07 == 0x05;
                if !self.sixteen_bit {
                    context.violation(
                        rules::PIXEL_FORMAT,
                        format!("pixel format {format:#04x} is not 16-bit RGB565"),
                    );
                }
            }
            _ => {}
        }
    }

    fn pixels(&mut self, context: &mut DeviceContext<'_>, bytes: &[u8]) {
        let mut bytes = bytes.iter().copied();
        let mut outside = 0_usize;
        loop {
            let high = match self.half_pixel.take() {
                Some(high) => high,
                None => match bytes.next() {
                    Some(high) => high,
                    None => break,
                },
            };
            let Some(low) = bytes.next() else {
                self.half_pixel = Some(high);
                break;
            };
            if !self.store(u16::from_be_bytes([high, low])) {
                outside += 1;
            }
        }
        if outside > 0 {
            context.violation(
                rules::PIXEL_OUTSIDE,
                format!("{outside} pixel(s) fell outside the panel"),
            );
        }
        self.changed();
    }

    /// Stores one pixel at the cursor and advances it through the window.
    fn store(&mut self, pixel: u16) -> bool {
        let (width, height) = self.addressed_size();
        let (column, page) = self.cursor;
        let stored = column < width && page < height;
        if stored {
            self.pixels[usize::from(page) * usize::from(width) + usize::from(column)] = pixel;
        }
        self.cursor = if column >= self.columns.1 {
            let page = if page >= self.pages.1 {
                self.pages.0
            } else {
                page + 1
            };
            (self.columns.0, page)
        } else {
            (column + 1, page)
        };
        stored
    }

    /// A pixel as the panel shows it.
    const fn shown(&self, pixel: u16) -> u16 {
        let pixel = if self.address_mode & BGR == 0 {
            pixel
        } else {
            (pixel & 0x07e0) | (pixel >> 11) | ((pixel & 0x1f) << 11)
        };
        if self.inverted {
            !pixel
        } else {
            pixel
        }
    }
}

impl SpiDevice for MipiDbiPanelModel {
    fn model(&self) -> &'static str {
        "mipi-dbi-panel"
    }

    fn transfer(
        &mut self,
        context: &mut DeviceContext<'_>,
        _frequency_hz: u32,
        write: &[u8],
        _read: &mut [u8],
    ) {
        match context.pin(self.dc) {
            Some(false) => {
                for command in write {
                    self.command(context, *command);
                }
            }
            Some(true) => self.data(context, write),
            None => context.violation(
                rules::NO_DC_PIN,
                format!("the D/C pin {} does not exist", self.dc),
            ),
        }
    }

    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
        let bytes: Vec<u8> = self
            .pixels
            .iter()
            .flat_map(|pixel| pixel.to_le_bytes())
            .collect();
        let source = crate::device::register_range(&bytes, offset, buffer.len())?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn frame(&self) -> Option<Frame> {
        let (width, height) = self.addressed_size();
        Some(Frame {
            width,
            height,
            visible: self.on && !self.sleeping,
            round: false,
            revision: self.revision,
            pixels: self.pixels.iter().map(|pixel| self.shown(*pixel)).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::spi::SpiBus as _;

    use super::*;
    use crate::{hardware::VirtualHardware, Clock, PinDrive, PinMode, PinPull};

    const DC: usize = 1;

    fn setup(panel: MipiDbiPanelModel) -> (VirtualHardware, crate::VirtualSpiBus) {
        let hardware =
            VirtualHardware::new(&["GPIO0", "GPIO1"], &[], Clock::manual()).with_spi(&["SPI0"]);
        hardware
            .attach_spi("SPI0", Box::new(panel))
            .expect("attach");
        hardware.configure_pin(DC, PinMode::Output, PinPull::None, PinDrive::PushPull, None);
        let bus = hardware.spi_bus("SPI0", 40_000_000).expect("bus");
        (hardware, bus)
    }

    fn send(hardware: &VirtualHardware, bus: &mut crate::VirtualSpiBus, command: u8, data: &[u8]) {
        hardware.set_output(DC, false);
        bus.write(&[command]).expect("command");
        if !data.is_empty() {
            hardware.set_output(DC, true);
            bus.write(data).expect("data");
        }
    }

    #[test]
    fn a_window_write_lands_where_the_driver_addressed_it() {
        let (hardware, mut bus) = setup(MipiDbiPanelModel::new(4, 3, (2, 0), DC));
        assert!(!hardware.display("SPI0").expect("frame").visible);
        send(&hardware, &mut bus, dcs::EXIT_SLEEP_MODE, &[]);
        send(&hardware, &mut bus, dcs::SET_PIXEL_FORMAT, &[0x55]);
        send(&hardware, &mut bus, dcs::SET_DISPLAY_ON, &[]);
        // Columns 1..=2 and page 1 after the 2-column panel offset.
        send(&hardware, &mut bus, dcs::SET_COLUMN_ADDRESS, &[0, 3, 0, 4]);
        send(&hardware, &mut bus, dcs::SET_PAGE_ADDRESS, &[0, 1, 0, 1]);
        // Pixels split across transfers, even mid-pixel.
        send(
            &hardware,
            &mut bus,
            dcs::WRITE_MEMORY_START,
            &[0xf8, 0x00, 0x07],
        );
        bus.write(&[0xe0]).expect("rest");

        let frame = hardware.display("SPI0").expect("frame");
        assert!(frame.visible);
        assert_eq!((frame.width, frame.height), (4, 3));
        assert_eq!(frame.pixels, [0, 0, 0, 0, 0, 0xf800, 0x07e0, 0, 0, 0, 0, 0]);
        assert!(hardware.violations().is_empty());
    }

    #[test]
    fn colour_order_inversion_and_exchange_change_what_is_shown() {
        let (hardware, mut bus) = setup(MipiDbiPanelModel::new(2, 1, (0, 0), DC));
        send(&hardware, &mut bus, dcs::WRITE_MEMORY_START, &[0xf8, 0x00]);
        assert_eq!(hardware.display("SPI0").expect("frame").pixels[0], 0xf800);

        send(&hardware, &mut bus, dcs::SET_ADDRESS_MODE, &[BGR]);
        assert_eq!(hardware.display("SPI0").expect("frame").pixels[0], 0x001f);
        send(&hardware, &mut bus, dcs::ENTER_INVERT_MODE, &[]);
        assert_eq!(hardware.display("SPI0").expect("frame").pixels[0], !0x001f);

        send(
            &hardware,
            &mut bus,
            dcs::SET_ADDRESS_MODE,
            &[ROW_COLUMN_EXCHANGE],
        );
        let frame = hardware.display("SPI0").expect("frame");
        assert_eq!((frame.width, frame.height), (1, 2));

        send(&hardware, &mut bus, dcs::SET_PIXEL_FORMAT, &[0x66]);
        assert_eq!(hardware.violations().len(), 1);
    }
}

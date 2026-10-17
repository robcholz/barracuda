//! Sitronix ST77916 round panel behind a quad-SPI link (ST77916 datasheet,
//! QSPI interface; the 360x360 panel of the ESP-VoCat).
//!
//! Each transfer starts with a one-line header: an opcode, then a 24-bit
//! address whose middle byte is the DCS command. Opcode 02h carries the
//! command's parameters; opcode 32h carries pixels on four lines after a
//! memory write. The DCS state machine is the MIPI DBI panel's, so windows,
//! address mode, and power behave the same way. The panel's IPS glass is
//! normally inverted: it shows true colors after an enter-invert command,
//! which is why its initialization sends one.

use crate::{
    device::{DeviceContext, RegisterRangeError},
    models::MipiDbiPanelModel,
    spi::{Frame, SpiDevice},
};

/// Native panel size.
pub const SIZE: u16 = 360;
/// Opcode of a register command and its parameters.
const COMMAND_WRITE: u8 = 0x02;
/// Opcode of a pixel-memory write.
const COLOR_WRITE: u8 = 0x32;
/// Header bytes: the opcode and the 24-bit address.
const HEADER: usize = 4;

mod rules {
    pub const HEADER: &str = "st77916/qspi-header";
}

/// An ST77916 panel.
#[derive(Debug)]
pub struct St77916PanelModel {
    panel: MipiDbiPanelModel,
}

impl St77916PanelModel {
    /// A 360x360 panel, asleep and off as after reset.
    #[must_use]
    pub fn new() -> Self {
        // The command travels in the header, so no D/C line is read.
        Self {
            panel: MipiDbiPanelModel::new(SIZE, SIZE, (0, 0), usize::MAX),
        }
    }
}

impl Default for St77916PanelModel {
    fn default() -> Self {
        Self::new()
    }
}

impl SpiDevice for St77916PanelModel {
    fn model(&self) -> &'static str {
        "st77916"
    }

    fn transfer(
        &mut self,
        context: &mut DeviceContext<'_>,
        _frequency_hz: u32,
        write: &[u8],
        _read: &mut [u8],
    ) {
        let Some((header, payload)) = write.split_at_checked(HEADER) else {
            context.violation(rules::HEADER, "a transfer shorter than its header");
            return;
        };
        let opcode = header[0];
        let command = header[2];
        if !matches!(opcode, COMMAND_WRITE | COLOR_WRITE) {
            context.violation(rules::HEADER, format!("unknown opcode {opcode:#04x}"));
            return;
        }
        self.panel.command(context, command);
        if !payload.is_empty() {
            self.panel.data(context, payload);
        }
    }

    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
        self.panel.peek(offset, buffer)
    }

    fn frame(&self) -> Option<Frame> {
        self.panel.frame().map(|frame| Frame {
            round: true,
            pixels: frame.pixels.into_iter().map(|pixel| !pixel).collect(),
            ..frame
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::spi::SpiBus as _;

    use super::*;
    use crate::{hardware::VirtualHardware, Clock};

    fn send(bus: &mut crate::VirtualSpiBus, opcode: u8, command: u8, data: &[u8]) {
        let mut bytes = vec![opcode, 0, command, 0];
        bytes.extend_from_slice(data);
        bus.write(&bytes).expect("write");
    }

    #[test]
    fn framed_commands_and_pixels_land_in_the_window() {
        let hardware = VirtualHardware::new(&[], &[], Clock::manual()).with_spi(&["SPI0"]);
        hardware
            .attach_spi("SPI0", Box::new(St77916PanelModel::new()))
            .expect("attach");
        let mut bus = hardware.spi_bus("SPI0", 40_000_000).expect("bus");
        send(&mut bus, COMMAND_WRITE, 0x3a, &[0x55]);
        send(&mut bus, COMMAND_WRITE, 0x21, &[]);
        send(&mut bus, COMMAND_WRITE, 0x11, &[]);
        send(&mut bus, COMMAND_WRITE, 0x29, &[]);
        send(&mut bus, COMMAND_WRITE, 0x2a, &[0, 1, 0, 1]);
        send(&mut bus, COMMAND_WRITE, 0x2b, &[0, 2, 0, 2]);
        send(&mut bus, COLOR_WRITE, 0x2c, &[0xf8, 0x00]);
        let frame = hardware.display("SPI0").expect("frame");
        assert_eq!((frame.width, frame.height, frame.round), (SIZE, SIZE, true));
        assert!(frame.visible);
        assert_eq!(frame.pixels[2 * usize::from(SIZE) + 1], 0xf800);
    }
}

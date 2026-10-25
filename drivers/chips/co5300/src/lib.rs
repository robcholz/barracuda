//! Command-level Chipone CO5300 QSPI AMOLED controller driver data.
//!
//! On a QSPI link every transfer starts with an opcode and a 24-bit address
//! on one data line. Register writes use [`COMMAND_WRITE_OPCODE`] with the
//! DCS command in the address's middle byte ([`command_address`]) and their
//! parameters on one line; pixel writes use [`COLOR_WRITE_OPCODE`] with
//! [`MEMORY_WRITE`] and send the pixels on all four lines.
//!
//! The CO5300 addresses its GRAM in pairs of pixels: a window must start on
//! an even column and row and span an even number of each, or the panel
//! shows the pixels shifted. [`Window::aligned`] widens a rectangle to that
//! grid.

#![no_std]

/// QSPI opcode for register commands and their parameters.
pub const COMMAND_WRITE_OPCODE: u8 = 0x02;
/// QSPI opcode for pixel-memory writes.
pub const COLOR_WRITE_OPCODE: u8 = 0x32;

/// DCS `SLPIN`.
pub const SLEEP_IN: u8 = 0x10;
/// DCS `SLPOUT`.
pub const SLEEP_OUT: u8 = 0x11;
/// DCS `DISPOFF`.
pub const DISPLAY_OFF: u8 = 0x28;
/// DCS `DISPON`.
pub const DISPLAY_ON: u8 = 0x29;
/// DCS `CASET`.
pub const COLUMN_ADDRESS: u8 = 0x2a;
/// DCS `RASET`.
pub const ROW_ADDRESS: u8 = 0x2b;
/// DCS `RAMWR`.
pub const MEMORY_WRITE: u8 = 0x2c;
/// DCS `MADCTL`.
pub const ADDRESS_MODE: u8 = 0x36;
/// DCS `COLMOD`.
pub const PIXEL_FORMAT: u8 = 0x3a;
/// DCS `WRDISBV`: display brightness, `0` darkest through `255`.
pub const WRITE_BRIGHTNESS: u8 = 0x51;

/// `COLMOD` value for 16-bit RGB565 pixels.
pub const PIXEL_FORMAT_RGB565: u8 = 0x55;
/// Time the controller needs after `SLPOUT` or `SLPIN`.
pub const SLEEP_SETTLE_MS: u32 = 150;

/// Returns the 24-bit QSPI address carrying one DCS command.
#[must_use]
pub const fn command_address(command: u8) -> u32 {
    u32::from_be_bytes([0, 0, command, 0])
}

/// One controller command and its required settling delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    /// Eight-bit DCS or vendor command.
    pub command: u8,
    /// Command parameters.
    pub data: &'static [u8],
    /// Delay required after the command completes.
    pub delay_ms: u16,
}

const fn command(command: u8, data: &'static [u8], delay_ms: u16) -> Command {
    Command {
        command,
        data,
        delay_ms,
    }
}

/// Bring-up of the 1.75-inch round AMOLED in the M5Stack StopWatch, from
/// M5Stack's M5GFX `Panel_StopWatch::getInitCommands`, with the 16-bit
/// `COLMOD` of M5GFX's `Panel_CO5300` base list.
pub const M5STACK_STOPWATCH_INIT: &[Command] = &[
    command(SLEEP_OUT, &[], 150),
    command(0xc4, &[0x80], 0),
    // Tearing effect on, V-blanking and H-blanking.
    command(0x35, &[0x80], 0),
    // Tearing scanline 0x1D2 = 466.
    command(0x44, &[0x01, 0xd2], 0),
    // Brightness control on.
    command(0x53, &[0x20], 0),
    // Inversion off.
    command(0x20, &[], 0),
    command(ADDRESS_MODE, &[0x00], 0),
    command(PIXEL_FORMAT, &[PIXEL_FORMAT_RGB565], 0),
    command(WRITE_BRIGHTNESS, &[0xa0], 0),
    command(DISPLAY_ON, &[], 0),
];

/// One GRAM window in controller coordinates, inclusive of its last pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// First column.
    pub x: u16,
    /// First row.
    pub y: u16,
    /// Columns covered.
    pub width: u16,
    /// Rows covered.
    pub height: u16,
}

impl Window {
    /// Widens a rectangle inside a `panel_width` x `panel_height` area to the
    /// controller's two-pixel grid: even start, even span.
    ///
    /// Returns `None` for an empty rectangle, one outside the area, or an
    /// area with an odd side, which no aligned window could cover.
    #[must_use]
    pub fn aligned(
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        panel_width: u16,
        panel_height: u16,
    ) -> Option<Self> {
        if panel_width & 1 != 0 || panel_height & 1 != 0 || width == 0 || height == 0 {
            return None;
        }
        let x_end = x.checked_add(width).filter(|end| *end <= panel_width)?;
        let y_end = y.checked_add(height).filter(|end| *end <= panel_height)?;
        let (x, x_end) = (x & !1, x_end.checked_add(1)? & !1);
        let (y, y_end) = (y & !1, y_end.checked_add(1)? & !1);
        Some(Self {
            x,
            y,
            width: x_end.checked_sub(x)?,
            height: y_end.checked_sub(y)?,
        })
    }

    /// Returns `CASET` parameters for this window placed `offset` columns
    /// into GRAM.
    #[must_use]
    pub fn column_parameters(self, offset: u16) -> Option<[u8; 4]> {
        span_parameters(self.x, self.width, offset)
    }

    /// Returns `RASET` parameters for this window placed `offset` rows into
    /// GRAM.
    #[must_use]
    pub fn row_parameters(self, offset: u16) -> Option<[u8; 4]> {
        span_parameters(self.y, self.height, offset)
    }
}

fn span_parameters(start: u16, length: u16, offset: u16) -> Option<[u8; 4]> {
    let first = start.checked_add(offset)?;
    let last = first.checked_add(length)?.checked_sub(1)?;
    let [first_high, first_low] = first.to_be_bytes();
    let [last_high, last_low] = last.to_be_bytes();
    Some([first_high, first_low, last_high, last_low])
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{
        DISPLAY_ON, M5STACK_STOPWATCH_INIT, PIXEL_FORMAT, PIXEL_FORMAT_RGB565, SLEEP_OUT, Window,
        command_address,
    };

    #[test]
    fn commands_ride_in_the_middle_address_byte() {
        assert_eq!(command_address(0x2c), 0x00_2c00);
        assert_eq!(command_address(0xff), 0x00_ff00);
    }

    #[test]
    fn windows_widen_to_the_two_pixel_grid() {
        // Already aligned.
        assert_eq!(
            Window::aligned(2, 4, 6, 8, 468, 468),
            Some(Window {
                x: 2,
                y: 4,
                width: 6,
                height: 8
            })
        );
        // One odd pixel grows to the pair holding it.
        assert_eq!(
            Window::aligned(3, 5, 1, 1, 468, 468),
            Some(Window {
                x: 2,
                y: 4,
                width: 2,
                height: 2
            })
        );
        // Odd start and odd end both widen.
        assert_eq!(
            Window::aligned(1, 0, 4, 3, 468, 468),
            Some(Window {
                x: 0,
                y: 0,
                width: 6,
                height: 4
            })
        );
        // The last pixel of an even panel stays inside it.
        assert_eq!(
            Window::aligned(467, 467, 1, 1, 468, 468),
            Some(Window {
                x: 466,
                y: 466,
                width: 2,
                height: 2
            })
        );
    }

    #[test]
    fn rejects_empty_outside_and_odd_areas() {
        assert_eq!(Window::aligned(0, 0, 0, 1, 468, 468), None);
        assert_eq!(Window::aligned(467, 0, 2, 1, 468, 468), None);
        assert_eq!(Window::aligned(0, 0, 1, 1, 467, 468), None);
    }

    #[test]
    fn window_parameters_include_the_gram_offset() {
        let window = Window::aligned(0, 0, 468, 2, 468, 468).expect("window");
        assert_eq!(window.column_parameters(6), Some([0x00, 0x06, 0x01, 0xd9]));
        assert_eq!(window.row_parameters(0), Some([0x00, 0x00, 0x00, 0x01]));
    }

    #[test]
    fn stopwatch_bring_up_wakes_selects_rgb565_and_ends_on() {
        let first = M5STACK_STOPWATCH_INIT.first().expect("commands");
        assert_eq!(first.command, SLEEP_OUT);
        assert!(first.delay_ms >= 120);
        assert!(M5STACK_STOPWATCH_INIT.iter().any(
            |command| command.command == PIXEL_FORMAT && command.data == [PIXEL_FORMAT_RGB565]
        ));
        assert_eq!(
            M5STACK_STOPWATCH_INIT.last().map(|command| command.command),
            Some(DISPLAY_ON)
        );
    }
}

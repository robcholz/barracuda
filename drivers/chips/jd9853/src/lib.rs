//! Command-level Jadard JD9853 TFT LCD controller driver data.
//!
//! The JD9853 speaks the MIPI DCS command set on its serial interface plus a
//! vendor register bank opened by `DFh 98h 53h` and paged by `DEh`. Its
//! 240 x 320 GRAM is addressed with the standard `CASET`/`RASET`/`RAMWR`.

#![no_std]

/// DCS `SWRESET`.
pub const SOFTWARE_RESET: u8 = 0x01;
/// DCS `SLPOUT`.
pub const SLEEP_OUT: u8 = 0x11;
/// DCS `DISPON`.
pub const DISPLAY_ON: u8 = 0x29;
/// DCS `MADCTL`.
pub const ADDRESS_MODE: u8 = 0x36;
/// DCS `COLMOD`.
pub const PIXEL_FORMAT: u8 = 0x3a;
/// `COLMOD` value selecting 16-bit RGB565 on the JD9853 (M5GFX
/// `Panel_JD9853::getColMod`; not the DCS `55h`).
pub const PIXEL_FORMAT_RGB565: u8 = 0x05;
/// GRAM columns.
pub const GRAM_WIDTH: u16 = 240;
/// GRAM rows.
pub const GRAM_HEIGHT: u16 = 320;

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

/// Bring-up of the HSD015BPN2-A0 240 x 240 panel in the M5Stack Chain
/// Captain, from LovyanGFX/M5GFX `Panel_JD9853::getInitCommands`. It leaves
/// `MADCTL` at `00h`, 16-bit pixels, and the display on.
pub const HSD015BPN2_INIT: &[Command] = &[
    command(SOFTWARE_RESET, &[], 20),
    // Open the vendor register bank.
    command(0xdf, &[0x98, 0x53], 0),
    command(0xde, &[0x00], 0),
    command(0xb2, &[0x38], 0),
    command(0xb7, &[0x00, 0x29, 0x00, 0x51], 0),
    command(0xbb, &[0x28, 0x2f, 0x55, 0x73, 0x63, 0xf0], 0),
    command(0xc0, &[0x22, 0xa2], 0),
    command(0xc1, &[0x12], 0),
    command(0xc3, &[0x7d, 0x08, 0x08, 0x0c, 0xc2, 0x72, 0x22, 0x88], 0),
    command(
        0xc4,
        &[
            0x00, 0x00, 0xa0, 0x79, 0x0a, 0x0b, 0x16, 0x79, 0x0a, 0x0b, 0x16, 0x82,
        ],
        0,
    ),
    // Gamma.
    command(
        0xc8,
        &[
            0x3f, 0x29, 0x23, 0x20, 0x27, 0x2d, 0x2a, 0x2b, 0x2a, 0x28, 0x25, 0x17, 0x12, 0x0d,
            0x07, 0x02, 0x3f, 0x29, 0x23, 0x20, 0x27, 0x2d, 0x2a, 0x2b, 0x2a, 0x28, 0x25, 0x17,
            0x12, 0x0d, 0x07, 0x02,
        ],
        0,
    ),
    command(0xd0, &[0x04, 0x04, 0x6a, 0x1c, 0x03], 0),
    command(0xd7, &[0x00, 0x20], 0),
    command(0xe6, &[0x10], 0),
    command(0xde, &[0x01], 0),
    command(0xbb, &[0x04], 0),
    command(0xd7, &[0x12], 0),
    command(0xb7, &[0x03, 0x13, 0xe5, 0x38, 0x38], 0),
    command(0xc1, &[0x14, 0x15, 0xc0], 0),
    command(0xc2, &[0x06, 0x3a], 0),
    command(0xc4, &[0x72, 0x12], 0),
    command(0xbe, &[0x00], 0),
    command(0xde, &[0x00], 0),
    // Tearing effect line on, V-blanking only.
    command(0x35, &[0x00], 0),
    command(ADDRESS_MODE, &[0x00], 0),
    command(PIXEL_FORMAT, &[PIXEL_FORMAT_RGB565], 0),
    command(0x2a, &[0x00, 0x00, 0x00, 0xef], 0),
    command(0x2b, &[0x00, 0x00, 0x00, 0xef], 0),
    command(SLEEP_OUT, &[], 120),
    command(DISPLAY_ON, &[], 10),
];

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::{
        DISPLAY_ON, HSD015BPN2_INIT, PIXEL_FORMAT, PIXEL_FORMAT_RGB565, SLEEP_OUT, SOFTWARE_RESET,
    };

    #[test]
    fn bring_up_resets_first_and_wakes_last() {
        assert_eq!(
            HSD015BPN2_INIT.first().map(|command| command.command),
            Some(SOFTWARE_RESET)
        );
        let tail: [u8; 2] = [SLEEP_OUT, DISPLAY_ON];
        let commands: [u8; 2] = core::array::from_fn(|index| {
            HSD015BPN2_INIT[HSD015BPN2_INIT.len() - 2 + index].command
        });
        assert_eq!(commands, tail);
        assert!(
            HSD015BPN2_INIT
                .iter()
                .any(|command| command.command == SLEEP_OUT && command.delay_ms >= 120)
        );
    }

    #[test]
    fn bring_up_selects_jd9853_rgb565_and_closes_the_vendor_bank() {
        assert!(HSD015BPN2_INIT.iter().any(
            |command| command.command == PIXEL_FORMAT && command.data == [PIXEL_FORMAT_RGB565]
        ));
        let last_page = HSD015BPN2_INIT
            .iter()
            .rev()
            .find(|command| command.command == 0xde)
            .map(|command| command.data);
        assert_eq!(last_page, Some(&[0x00][..]));
        assert_eq!(
            HSD015BPN2_INIT
                .iter()
                .find(|command| command.command == 0xc8)
                .map(|command| command.data.len()),
            Some(32)
        );
    }
}
